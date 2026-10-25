use anyhow::{Context, Result};
use memmap2::MmapMut;
use nix::{
    sys::memfd::{memfd_create, MemFdCreateFlag},
    unistd::ftruncate,
};
use std::{ffi::CString, fs::File, io::Write};
use tiny_skia::{BlendMode, PixmapMut, PixmapPaint, PixmapRef, Transform};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_keyboard::{self, WlKeyboard},
    wl_output::WlOutput,
    wl_pointer::WlPointer,
    wl_registry::{self, WlRegistry},
    wl_seat::{self, Capability, WlSeat},
    wl_shm::{Format, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{
    delegate_dispatch, delegate_noop, Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{self, ZxdgOutputV1},
};
use wayland_protocols_wlr::{
    layer_shell::v1::client::{
        zwlr_layer_shell_v1::ZwlrLayerShellV1,
        zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
    },
    screencopy::v1::client::{
        zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
        zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
    },
};

use crate::cursor::{Coordinate, CursorState};
use crate::render::{create_buffer_from_memfd, DimmingBuffer, SurfaceLayer};

#[derive(Debug, Clone, Copy)]
pub struct OutputInfo {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: Format,
}

impl Default for OutputInfo {
    fn default() -> Self {
        OutputInfo {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            stride: 0,
            format: Format::Abgr8888,
        }
    }
}

pub struct Globals {
    pub output_manager: ZxdgOutputManagerV1,
    pub screencopy_manager: ZwlrScreencopyManagerV1,
    pub shm: WlShm,
    pub compositor: WlCompositor,
    pub layer_shell: ZwlrLayerShellV1,
}

pub struct Output {
    pub proxy: WlOutput,
    pub info: OutputInfo,
    pub dimming_layer: Option<SurfaceLayer>,
    pub dimming_buffer: Option<DimmingBuffer>,
}

#[derive(Default)]
pub struct App {
    zxdg_output_manager: Option<ZxdgOutputManagerV1>,
    screencopy_manager: Option<ZwlrScreencopyManagerV1>,
    shm: Option<WlShm>,
    compositor: Option<WlCompositor>,
    layer_shell: Option<ZwlrLayerShellV1>,

    pub outputs: Vec<Output>,
    pub exit_requested: bool,
    pub cursor: CursorState,
    layers_configured: usize,
    screenshots_captured: usize,
}

delegate_noop!(App: ignore ZxdgOutputManagerV1);
delegate_noop!(App: ignore ZwlrScreencopyManagerV1);
delegate_noop!(App: ignore WlShm);
delegate_noop!(App: ignore WlShmPool);
delegate_noop!(App: ignore WlOutput);
delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore ZwlrLayerShellV1);
delegate_noop!(App: ignore WlSurface);

delegate_dispatch!(App: [WlPointer: ()] => CursorState);
delegate_dispatch!(App: [WlBuffer: usize] => DimmingBuffer);
delegate_dispatch!(App: [WlCallback: usize] => DimmingBuffer);

impl Dispatch<WlKeyboard, ()> for App {
    fn event(
        state: &mut Self,
        _keyboard: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key {
            key,
            state: key_state,
            ..
        } = event
        {
            if key == 1 {
                if let WEnum::Value(wl_keyboard::KeyState::Pressed) = key_state {
                    state.exit_requested = true;
                }
            }
        }
    }
}

impl Dispatch<WlRegistry, ()> for App {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_output" => {
                    state.outputs.push(Output {
                        proxy: registry.bind::<WlOutput, _, _>(name, version, qh, ()),
                        info: OutputInfo::default(),
                        dimming_layer: None,
                        dimming_buffer: None,
                    });
                }
                "zxdg_output_manager_v1" => {
                    state.zxdg_output_manager = Some(registry.bind(name, version.min(2), qh, ()));
                }
                "zwlr_screencopy_manager_v1" => {
                    state.screencopy_manager = Some(registry.bind(name, version.min(3), qh, ()));
                }
                "wl_shm" => {
                    state.shm = Some(registry.bind(name, version, qh, ()));
                }
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "wl_seat" => {
                    registry.bind::<WlSeat, _, _>(name, version.min(8), qh, ());
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, usize> for App {
    fn event(
        state: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        output: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format: WEnum::Value(format),
                width,
                height,
                stride,
            } => {
                let info = &mut state.outputs[*output].info;
                info.width = width;
                info.height = height;
                info.stride = stride;
                info.format = format;
            }
            zwlr_screencopy_frame_v1::Event::Ready { .. } => {
                state.screenshots_captured += 1;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZxdgOutputV1, usize> for App {
    fn event(
        state: &mut Self,
        _proxy: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        data: &usize,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let info = &mut state.outputs[*data].info;
        if let zxdg_output_v1::Event::LogicalPosition { x, y } = event {
            info.x = x;
            info.y = y;
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for App {
    fn event(
        state: &mut Self,
        layer_surface: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = event {
            layer_surface.ack_configure(serial);
            state.layers_configured += 1;
        }
    }
}

impl Dispatch<WlSeat, ()> for App {
    fn event(
        _: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            if caps.contains(Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
            if caps.contains(Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl App {
    pub fn run(&mut self) -> Result<()> {
        let conn = Connection::connect_to_env().context("Failed to connect to Wayland")?;
        let mut event_queue = conn.new_event_queue();
        let qh = event_queue.handle();

        let Globals {
            output_manager,
            screencopy_manager,
            shm,
            compositor,
            layer_shell,
        } = self.extract_globals(&conn, &mut event_queue)?;

        for (i, output) in self.outputs.iter_mut().enumerate() {
            output_manager.get_xdg_output(&output.proxy, &qh, i);
        }

        let screenshot_memfds =
            self.capture_screenshots(&screencopy_manager, &shm, &qh, &mut event_queue)?;

        self.create_layers_and_buffers(
            &compositor,
            &layer_shell,
            &shm,
            &screenshot_memfds,
            &qh,
            &mut event_queue,
        )?;

        while !self.exit_requested {
            event_queue.blocking_dispatch(self)?;
            event_queue.flush()?;

            for (i, output) in self.outputs.iter_mut().enumerate() {
                if let Some(ref mut dimming) = output.dimming_buffer {
                    if dimming.needs_render {
                        let cursor_start = self.cursor.start().unwrap_or_default();
                        dimming.update(
                            output.dimming_layer.as_ref().unwrap(),
                            &qh,
                            i,
                            &output.info,
                            cursor_start,
                            self.cursor.global_pos(),
                        )?;
                    }
                }
            }
        }

        event_queue.flush()?;

        if let Some((start, end)) = self.cursor.selection() {
            let png_data = self.compose_and_export_screenshot(&screenshot_memfds, start, end)?;
            std::io::stdout().write_all(&png_data)?;
        }

        Ok(())
    }

    fn extract_globals(
        &mut self,
        conn: &Connection,
        event_queue: &mut EventQueue<Self>,
    ) -> Result<Globals> {
        conn.display().get_registry(&event_queue.handle(), ());
        event_queue.roundtrip(self)?;

        Ok(Globals {
            output_manager: self
                .zxdg_output_manager
                .take()
                .context("Could not obtain xdg output manager!")?,
            screencopy_manager: self
                .screencopy_manager
                .take()
                .context("Could not obtain screencopy manager!")?,
            shm: self.shm.take().context("Could not get shm!")?,
            compositor: self
                .compositor
                .take()
                .context("Could not obtain compositor!")?,
            layer_shell: self
                .layer_shell
                .take()
                .context("Could not obtain layer shell!")?,
        })
    }

    fn capture_screenshots(
        &mut self,
        screencopy_manager: &ZwlrScreencopyManagerV1,
        shm: &WlShm,
        qh: &QueueHandle<Self>,
        event_queue: &mut EventQueue<Self>,
    ) -> Result<Vec<File>> {
        let frames: Vec<ZwlrScreencopyFrameV1> = self
            .outputs
            .iter()
            .enumerate()
            .map(|(i, output)| screencopy_manager.capture_output(0, &output.proxy, qh, i))
            .collect();

        event_queue.roundtrip(self)?;

        let screenshot_memfds: Vec<File> = self
            .outputs
            .iter_mut()
            .enumerate()
            .map(|(i, output)| {
                let memfd =
                    memfd_create(&CString::new("wayland-shm")?, MemFdCreateFlag::MFD_CLOEXEC)?;
                ftruncate(&memfd, (output.info.stride * output.info.height) as i64)?;
                let buffer = create_buffer_from_memfd(
                    shm,
                    &memfd,
                    &output.info,
                    qh,
                    i,
                    Some(output.info.format),
                )?;
                frames[i].copy(&buffer);
                Ok::<File, anyhow::Error>(memfd.into())
            })
            .collect::<Result<_>>()?;

        while self.screenshots_captured < self.outputs.len() {
            event_queue.roundtrip(self)?;
        }

        Ok(screenshot_memfds)
    }

    fn create_layers_and_buffers(
        &mut self,
        compositor: &WlCompositor,
        layer_shell: &ZwlrLayerShellV1,
        shm: &WlShm,
        screenshot_memfds: &[File],
        qh: &QueueHandle<Self>,
        event_queue: &mut EventQueue<Self>,
    ) -> Result<()> {
        let screenshot_layers: Vec<SurfaceLayer> = self
            .outputs
            .iter()
            .enumerate()
            .map(|(i, output)| {
                SurfaceLayer::new(compositor, layer_shell, &output.proxy, qh, "freeze", i)
            })
            .collect();

        for (i, output) in self.outputs.iter_mut().enumerate() {
            output.dimming_layer = Some(SurfaceLayer::new(
                compositor,
                layer_shell,
                &output.proxy,
                qh,
                "freeze-dimming",
                i,
            ));
            let offset = Coordinate {
                x: output.info.x,
                y: output.info.y,
            };
            let surface_id = output.dimming_layer.as_ref().unwrap().surface.id();
            self.cursor.register_surface(surface_id, offset);
        }

        let expected_layers = self.outputs.len() * 2;
        while self.layers_configured < expected_layers {
            event_queue.roundtrip(self)?;
        }

        for (i, output) in self.outputs.iter_mut().enumerate() {
            let screenshot_buffer = create_buffer_from_memfd(
                shm,
                &screenshot_memfds[i],
                &output.info,
                qh,
                i,
                Some(Format::Abgr8888),
            )?;
            screenshot_layers[i].attach_buffer(
                &screenshot_buffer,
                output.info.width as i32,
                output.info.height as i32,
            );
            let mut dimming = DimmingBuffer::new(shm, qh, &output.info, i)?;
            let pos = Coordinate::default();
            dimming.render(pos, pos)?;
            dimming.commit(output.dimming_layer.as_ref().unwrap(), qh, i);
            output.dimming_buffer = Some(dimming);
        }

        Ok(())
    }

    fn compose_and_export_screenshot(
        &self,
        screenshot_memfds: &[File],
        start: Coordinate,
        end: Coordinate,
    ) -> Result<Vec<u8>> {
        let sel_x = start.x.min(end.x);
        let sel_y = start.y.min(end.y);
        let sel_w = (start.x.max(end.x) - start.x.min(end.x)) as u32;
        let sel_h = (start.y.max(end.y) - start.y.min(end.y)) as u32;

        let mut dest_data = vec![0u8; (sel_w * sel_h * 4) as usize];
        let mut dest_pixmap = PixmapMut::from_bytes(dest_data.as_mut_slice(), sel_w, sel_h)
            .context("Failed to create destination pixmap")?;

        for (i, output) in self.outputs.iter().enumerate() {
            let out_x = output.info.x;
            let out_y = output.info.y;
            let out_w = output.info.width;
            let out_h = output.info.height;

            let mmap = unsafe { MmapMut::map_mut(&screenshot_memfds[i])? };

            if let Some(src_pixmap) = PixmapRef::from_bytes(mmap.as_ref(), out_w, out_h) {
                let draw_x = out_x - sel_x;
                let draw_y = out_y - sel_y;

                let paint = PixmapPaint {
                    blend_mode: BlendMode::Source,
                    ..PixmapPaint::default()
                };

                dest_pixmap.draw_pixmap(
                    draw_x,
                    draw_y,
                    src_pixmap,
                    &paint,
                    Transform::identity(),
                    None,
                );
            }
        }

        dest_pixmap
            .as_ref()
            .encode_png()
            .context("Failed to encode PNG")
    }
}
