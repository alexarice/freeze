use anyhow::{anyhow, Result};
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, Capability, WlSeat};
use wayland_client::protocol::wl_shm::{Format, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::ZxdgOutputV1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::ZwlrLayerSurfaceV1;
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1;
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1;
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

use std::fs::File;

use crate::DimmingBuffer;
use crate::SurfaceLayer;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CursorPosition {
    pub x: f64,
    pub y: f64,
}

#[derive(Default)]
pub struct OutputInfo {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

pub struct BufferInfo {
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
}

pub struct Output {
    pub proxy: WlOutput,
    pub info: OutputInfo,
    pub buffer_info: Option<BufferInfo>,
    pub frame: Option<ZwlrScreencopyFrameV1>,
    pub screenshot_captured: bool,
    pub screenshot_memfd: Option<File>,
    pub screenshot_layer: Option<SurfaceLayer>,
    pub dimming_layer: Option<SurfaceLayer>,
    pub screenshot_buffer: Option<WlBuffer>,
    pub dimming_buffer: Option<DimmingBuffer>,
}

impl Output {
    pub fn from_proxy(proxy: WlOutput) -> Self {
        Output {
            proxy,
            frame: None,
            info: Default::default(),
            buffer_info: Default::default(),
            screenshot_captured: false,
            screenshot_memfd: None,
            screenshot_layer: None,
            dimming_layer: None,
            screenshot_buffer: None,
            dimming_buffer: None,
        }
    }
}

#[derive(Default)]
pub struct Globals {
    pub zxdg_output_manager: Option<ZxdgOutputManagerV1>,
    pub screencopy_manager: Option<ZwlrScreencopyManagerV1>,
    pub shm: Option<WlShm>,
    pub compositor: Option<WlCompositor>,
    pub layer_shell: Option<ZwlrLayerShellV1>,
}

impl Globals {
    pub fn zxdg_output_manager(&self) -> Result<&ZxdgOutputManagerV1> {
        self.zxdg_output_manager
            .as_ref()
            .ok_or(anyhow!("Could not obtain xdg output manager!"))
    }

    pub fn screencopy_manager(&self) -> Result<&ZwlrScreencopyManagerV1> {
        self.screencopy_manager
            .as_ref()
            .ok_or(anyhow!("Could not obtain screencopy manager!"))
    }

    pub fn shm(&self) -> Result<&WlShm> {
        self.shm.as_ref().ok_or(anyhow!("Could not get shm!"))
    }

    pub fn compositor(&self) -> Result<&WlCompositor> {
        self.compositor
            .as_ref()
            .ok_or(anyhow!("Could not obtain compositor!"))
    }

    pub fn layer_shell(&self) -> Result<&ZwlrLayerShellV1> {
        self.layer_shell
            .as_ref()
            .ok_or(anyhow!("Could not obtain layer shell!"))
    }
}

#[derive(Default)]
pub struct State {
    pub globals: Globals,
    pub outputs: Vec<Output>,
    pub exit_requested: bool,
    pub cursor: CursorPosition,
    pub cursor_start: Option<CursorPosition>,
    pub cursor_end: Option<CursorPosition>,
    pub active_output_idx: usize,
}

delegate_noop!(State: ignore ZxdgOutputManagerV1);
delegate_noop!(State: ignore ZwlrScreencopyManagerV1);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlOutput);
delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore ZwlrLayerShellV1);
delegate_noop!(State: ignore WlSurface);

impl Dispatch<WlRegistry, ()> for State {
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
                    state.globals.zxdg_output_manager =
                        Some(registry.bind(name, version.min(2), qh, ()));
                }
                "zwlr_screencopy_manager_v1" => {
                    state.globals.screencopy_manager =
                        Some(registry.bind(name, version.min(3), qh, ()));
                }
                "wl_shm" => {
                    state.globals.shm = Some(registry.bind(name, version, qh, ()));
                }
                "wl_compositor" => {
                    state.globals.compositor = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "zwlr_layer_shell_v1" => {
                    state.globals.layer_shell = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "wl_seat" => {
                    registry.bind::<WlSeat, _, _>(name, version.min(8), qh, ());
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, usize> for State {
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
                state.outputs[*output].buffer_info = Some(BufferInfo {
                    format,
                    width,
                    height,
                    stride,
                });
            }
            zwlr_screencopy_frame_v1::Event::Ready { .. } => {
                state.outputs[*output].screenshot_captured = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZxdgOutputV1, usize> for State {
    fn event(
        state: &mut Self,
        _proxy: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        data: &usize,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let info = &mut state.outputs[*data].info;
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                info.x = x;
                info.y = y;
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                info.width = width;
                info.height = height;
            }
            zxdg_output_v1::Event::Done => {}
            zxdg_output_v1::Event::Name { name } if !name.is_empty() => {
                info.name = name;
            }
            _ => {}
        }
    }
}

impl Dispatch<WlBuffer, usize> for State {
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
                    dimming.state = crate::BufferState::Released;
                }
            }
        }
    }
}

impl Dispatch<WlCallback, usize> for State {
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

impl Dispatch<ZwlrLayerSurfaceV1, usize> for State {
    fn event(
        state: &mut Self,
        layer_surface: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        output_idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = event {
            layer_surface.ack_configure(serial);

            if let Some(ref mut layer) = state.outputs[*output_idx].screenshot_layer {
                if layer.layer_surface == *layer_surface {
                    layer.configured = true;
                    return;
                }
            }
            if let Some(ref mut layer) = state.outputs[*output_idx].dimming_layer {
                if layer.layer_surface == *layer_surface {
                    layer.configured = true;
                }
            }
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
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

impl Dispatch<WlPointer, ()> for State {
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
                for (i, output) in state.outputs.iter().enumerate() {
                    if let Some(ref dimming_layer) = output.dimming_layer {
                        if dimming_layer.surface == surface {
                            state.active_output_idx = i;
                            break;
                        }
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

impl Dispatch<WlKeyboard, ()> for State {
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
