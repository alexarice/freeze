use anyhow::{Context, Result};
use memmap2::MmapMut;
use nix::{
    sys::memfd::{memfd_create, MemFdCreateFlag},
    unistd::ftruncate,
};
use std::{ffi::CString, fs::File, io::Write};
use tiny_skia::{BlendMode, PixmapMut, PixmapPaint, PixmapRef, Transform};
use wayland_client::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_callback::{self, WlCallback},
    wl_compositor::WlCompositor,
    wl_keyboard::{self, WlKeyboard},
    wl_output::WlOutput,
    wl_pointer::{self, WlPointer},
    wl_registry::{self, WlRegistry},
    wl_seat::{self, Capability, WlSeat},
    wl_shm::{Format, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle, WEnum};
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

use crate::buffer::{create_buffer_from_memfd, DimmingBuffer, SurfaceLayer};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CursorPosition {
    pub x: f64,
    pub y: f64,
}

pub struct OutputInfo {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: Format,
}

pub struct Layers {
    pub screenshot: SurfaceLayer,
    pub dimming: SurfaceLayer,
}

pub struct Output {
    pub proxy: WlOutput,
    pub info: OutputInfo,
    pub screenshot_buffer: Option<WlBuffer>,
    pub dimming_buffer: Option<DimmingBuffer>,
}

impl Output {
    pub fn from_proxy(proxy: WlOutput) -> Self {
        Output {
            proxy,
            info: OutputInfo {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
                stride: 0,
                format: Format::Abgr8888,
            },
            screenshot_buffer: None,
            dimming_buffer: None,
        }
    }
}

#[derive(Default)]
pub struct App {
    // Globals (initialized during setup, unwrapped after)
    zxdg_output_manager: Option<ZxdgOutputManagerV1>,
    screencopy_manager: Option<ZwlrScreencopyManagerV1>,
    shm: Option<WlShm>,
    compositor: Option<WlCompositor>,
    layer_shell: Option<ZwlrLayerShellV1>,

    // State
    pub outputs: Vec<Output>,
    pub exit_requested: bool,
    pub cursor: CursorPosition,
    pub cursor_start: Option<CursorPosition>,
    pub cursor_end: Option<CursorPosition>,
    pub active_output_idx: usize,
    layers_configured: usize,
    screenshots_captured: usize,
    layers: Vec<Layers>,
}

delegate_noop!(App: ignore ZxdgOutputManagerV1);
delegate_noop!(App: ignore ZwlrScreencopyManagerV1);
delegate_noop!(App: ignore WlShm);
delegate_noop!(App: ignore WlShmPool);
delegate_noop!(App: ignore WlOutput);
delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore ZwlrLayerShellV1);
delegate_noop!(App: ignore WlSurface);

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
                    let info =
                        Output::from_proxy(registry.bind::<WlOutput, _, _>(name, version, qh, ()));
                    state.outputs.push(info);
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

impl Dispatch<WlBuffer, usize> for App {
    fn event(
        state: &mut Self,
        _buffer: &WlBuffer,
        event: wl_buffer::Event,
        output_idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            if let Some(output) = state.outputs.get_mut(*output_idx) {
                if let Some(ref mut dimming) = output.dimming_buffer {
                    dimming.state = crate::buffer::BufferState::Released;
                }
            }
        }
    }
}

impl Dispatch<WlCallback, usize> for App {
    fn event(
        state: &mut Self,
        _callback: &WlCallback,
        event: wl_callback::Event,
        output_idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            if let Some(output) = state.outputs.get_mut(*output_idx) {
                if let Some(ref mut dimming) = output.dimming_buffer {
                    dimming.frame_callback = None;
                }
            }
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

impl Dispatch<WlPointer, ()> for App {
    fn event(
        state: &mut Self,
        _pointer: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                serial: _,
                surface,
            } => {
                for (i, layers) in state.layers.iter().enumerate() {
                    if layers.dimming.surface == surface {
                        state.active_output_idx = i;
                        break;
                    }
                }

                state.cursor = CursorPosition {
                    x: surface_x,
                    y: surface_y,
                };
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                let global_pos = if let Some(output) = state.outputs.get(state.active_output_idx) {
                    CursorPosition {
                        x: surface_x + output.info.x as f64,
                        y: surface_y + output.info.y as f64,
                    }
                } else {
                    CursorPosition {
                        x: surface_x,
                        y: surface_y,
                    }
                };

                state.cursor = global_pos;

                if state.cursor_start.is_some() {
                    for output in state.outputs.iter_mut() {
                        if let Some(ref mut dimming) = output.dimming_buffer {
                            dimming.needs_render = true;
                        }
                    }
                }
            }
            wl_pointer::Event::Button {
                state: btn_state, ..
            } => match btn_state {
                WEnum::Value(wl_pointer::ButtonState::Pressed) => {
                    state.cursor_start = Some(state.cursor);
                }
                WEnum::Value(wl_pointer::ButtonState::Released) => {
                    state.cursor_end = Some(state.cursor);
                    state.exit_requested = true;
                }
                _ => {}
            },
            _ => {}
        }
    }
}

impl App {
    pub fn run(&mut self) -> Result<()> {
        let conn = Connection::connect_to_env().context("Failed to connect to Wayland")?;
        let mut event_queue = conn.new_event_queue();
        let qh = event_queue.handle();
        conn.display().get_registry(&qh, ());

        event_queue.roundtrip(self)?;

        // Extract all globals after registry setup - guaranteed to be Some after roundtrip
        let output_manager = self
            .zxdg_output_manager
            .take()
            .context("Could not obtain xdg output manager!")?;
        let screencopy_manager = self
            .screencopy_manager
            .take()
            .context("Could not obtain screencopy manager!")?;
        let shm = self.shm.take().context("Could not get shm!")?;
        let compositor = self
            .compositor
            .take()
            .context("Could not obtain compositor!")?;
        let layer_shell = self
            .layer_shell
            .take()
            .context("Could not obtain layer shell!")?;

        // Request xdg output info and screencopy frames
        let frames: Vec<ZwlrScreencopyFrameV1> = self
            .outputs
            .iter()
            .enumerate()
            .map(|(i, output)| screencopy_manager.capture_output(0, &output.proxy, &qh, i))
            .collect();

        for (i, output) in self.outputs.iter_mut().enumerate() {
            output_manager.get_xdg_output(&output.proxy, &qh, i);
        }

        event_queue.roundtrip(self)?;

        // Allocate screenshot buffers and collect memfds
        let screenshot_memfds: Vec<File> = self
            .outputs
            .iter_mut()
            .enumerate()
            .map(|(i, output)| {
                let memfd =
                    memfd_create(&CString::new("wayland-shm")?, MemFdCreateFlag::MFD_CLOEXEC)?;
                ftruncate(&memfd, (output.info.stride * output.info.height) as i64)?;
                let buffer = create_buffer_from_memfd(
                    &shm,
                    &memfd,
                    &output.info,
                    &qh,
                    i,
                    Some(output.info.format),
                )?;
                frames[i].copy(&buffer);
                Ok::<File, anyhow::Error>(memfd.into())
            })
            .collect::<Result<_>>()?;

        // Wait for all screenshots to be captured
        while self.screenshots_captured < self.outputs.len() {
            event_queue.roundtrip(self)?;
        }

        // Create layer surfaces
        let layers: Vec<Layers> = self
            .outputs
            .iter()
            .enumerate()
            .map(|(i, output)| Layers {
                screenshot: SurfaceLayer::new(
                    &compositor,
                    &layer_shell,
                    &output.proxy,
                    &qh,
                    "simple-ss",
                    i,
                ),
                dimming: SurfaceLayer::new(
                    &compositor,
                    &layer_shell,
                    &output.proxy,
                    &qh,
                    "simple-ss-dimming",
                    i,
                ),
            })
            .collect();
        self.layers = layers;

        // Wait for all layer surfaces to be configured (2 per output: screenshot + dimming)
        let expected_layers = self.outputs.len() * 2;
        while self.layers_configured < expected_layers {
            event_queue.roundtrip(self)?;
        }

        // Create buffers for screenshot and dimming overlay
        for (i, output) in self.outputs.iter_mut().enumerate() {
            let screenshot_buffer = create_buffer_from_memfd(
                &shm,
                &screenshot_memfds[i],
                &output.info,
                &qh,
                i,
                Some(Format::Abgr8888),
            )?;
            output.screenshot_buffer = Some(screenshot_buffer);
            output.dimming_buffer = Some(DimmingBuffer::new(&shm, &qh, &output.info, i)?);
        }

        // Attach screenshot buffers
        for (i, output) in self.outputs.iter_mut().enumerate() {
            let buffer = output
                .screenshot_buffer
                .as_ref()
                .context("Buffer not created!")?;

            self.layers[i].screenshot.surface.attach(Some(buffer), 0, 0);
            self.layers[i].screenshot.surface.damage_buffer(
                0,
                0,
                output.info.width as i32,
                output.info.height as i32,
            );
            self.layers[i].screenshot.surface.commit();
        }

        // Initial render of dimming overlay (fully dimmed, no cutout)
        for (i, output) in self.outputs.iter_mut().enumerate() {
            if let Some(ref mut dimming) = output.dimming_buffer {
                let pos = CursorPosition { x: 0.0, y: 0.0 };
                dimming.render(pos, pos)?;
                dimming.commit(&self.layers[i].dimming.surface, &qh, i);
            }
        }

        while !self.exit_requested {
            event_queue.blocking_dispatch(self)?;
            event_queue.flush()?;

            for (i, output) in self.outputs.iter_mut().enumerate() {
                if let Some(ref mut dimming) = output.dimming_buffer {
                    let cursor_start = self.cursor_start.unwrap_or_default();
                    dimming.update(
                        &self.layers[i].dimming.surface,
                        &qh,
                        i,
                        &output.info,
                        cursor_start,
                        self.cursor,
                    )?;
                }
            }
        }

        event_queue.flush()?;

        if let (Some(start), Some(end)) = (self.cursor_start, self.cursor_end) {
            let sel_x = start.x.min(end.x) as i32;
            let sel_y = start.y.min(end.y) as i32;
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

            if let Ok(png_data) = dest_pixmap.as_ref().encode_png() {
                std::io::stdout().write_all(&png_data)?;
            }
        }

        Ok(())
    }
}

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
