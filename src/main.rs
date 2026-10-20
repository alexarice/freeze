use anyhow::{anyhow, Context, Result};
use clap::Parser;
use memmap2::MmapMut;
use nix::sys::memfd::{memfd_create, MemFdCreateFlag};
use nix::unistd::ftruncate;
use std::ffi::CString;
use std::fs::File;
use std::os::fd::AsFd;

use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_compositor::{self, WlCompositor};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, Capability, WlSeat};
use wayland_client::protocol::wl_shm::{self, Format, WlShm};
use wayland_client::protocol::wl_shm_pool::{self, WlShmPool};
use wayland_client::protocol::wl_surface::{self, WlSurface};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::{
    self, ZxdgOutputManagerV1,
};
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::{self, ZxdgOutputV1};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{
    self, Layer, ZwlrLayerShellV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1,
};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::{
    self, ZwlrScreencopyFrameV1,
};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::{
    self, ZwlrScreencopyManagerV1,
};

#[derive(Parser)]
#[command(name = "simple-ss")]
#[command(about = "A minimal screenshot tool for Wayland", long_about = None)]
struct Cli {}

struct SurfaceLayer {
    surface: WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
    configured: bool,
}

struct DimmingDoubleBuffer {
    memfd_front: File,
    memfd_back: File,
    buffer_front: WlBuffer,
    buffer_back: WlBuffer,
    back_ready: bool,
}

impl DimmingDoubleBuffer {
    fn new(
        shm: &WlShm,
        qh: &QueueHandle<State>,
        buffer_info: &BufferInfo,
        surface: &WlSurface,
    ) -> Result<Self> {
        let width = buffer_info.width as i32;
        let height = buffer_info.height as i32;
        let stride = buffer_info.stride as i32;
        let format = Format::Abgr8888;

        // Create first memfd
        let memfd_a = memfd_create(
            &CString::new("wayland-shm-dimming-a")?,
            MemFdCreateFlag::MFD_CLOEXEC,
        )?;
        let buffer_size = (width * stride) as i64;
        ftruncate(&memfd_a, buffer_size)?;

        // Create second memfd
        let memfd_b = memfd_create(
            &CString::new("wayland-shm-dimming-b")?,
            MemFdCreateFlag::MFD_CLOEXEC,
        )?;
        ftruncate(&memfd_b, buffer_size)?;

        // Memory-map and fill front buffer with alpha=128 (50% transparent black)
        let mut mmap_a = unsafe { MmapMut::map_mut(&memfd_a)? };
        for i in (3..buffer_size as usize).step_by(4) {
            mmap_a[i] = 128;
        }

        // Create buffer pools
        let pool_a = shm.create_pool(memfd_a.as_fd(), buffer_size as i32, qh, ());
        let pool_b = shm.create_pool(memfd_b.as_fd(), buffer_size as i32, qh, ());

        // Create buffers
        let buffer_a = pool_a.create_buffer(0, width, height, stride, format, qh, ());
        let buffer_b = pool_b.create_buffer(0, width, height, stride, format, qh, ());

        // Attach front buffer immediately
        surface.attach(Some(&buffer_a), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.commit();

        Ok(DimmingDoubleBuffer {
            memfd_front: memfd_a.into(),
            memfd_back: memfd_b.into(),
            buffer_front: buffer_a,
            buffer_back: buffer_b,
            back_ready: true,
        })
    }
}

struct Output {
    proxy: WlOutput,
    info: OutputInfo,
    buffer_info: Option<BufferInfo>,
    frame: Option<ZwlrScreencopyFrameV1>,
    screenshot_captured: bool,
    screenshot_memfd: Option<File>,
    screenshot_layer: Option<SurfaceLayer>,
    dimming_layer: Option<SurfaceLayer>,
    screenshot_buffer: Option<WlBuffer>,
    dimming_double_buffer: Option<DimmingDoubleBuffer>,
}

#[derive(Default)]
struct OutputInfo {
    name: String,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

struct BufferInfo {
    format: Format,
    width: u32,
    height: u32,
    stride: u32,
}

impl Output {
    fn from_proxy(proxy: WlOutput) -> Self {
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
            dimming_double_buffer: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct CursorPosition {
    x: f64,
    y: f64,
}

struct State {
    zxdg_output_manager: Option<ZxdgOutputManagerV1>,
    screencopy_manager: Option<ZwlrScreencopyManagerV1>,
    shm: Option<WlShm>,
    compositor: Option<WlCompositor>,
    layer_shell: Option<ZwlrLayerShellV1>,
    seat: Option<WlSeat>,
    outputs: Vec<Output>,
    exit_requested: bool,
    cursor: CursorPosition,
    cursor_start: Option<CursorPosition>,
    cursor_end: Option<CursorPosition>,
    committed_mouse: Option<CursorPosition>,
}

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
            if interface == "wl_output" {
                let info =
                    Output::from_proxy(registry.bind::<WlOutput, (), State>(name, version, qh, ()));
                state.outputs.push(info);
            } else if interface == "zxdg_output_manager_v1" {
                state.zxdg_output_manager = Some(registry.bind::<ZxdgOutputManagerV1, (), State>(
                    name,
                    version.min(2),
                    qh,
                    (),
                ));
            } else if interface == "zwlr_screencopy_manager_v1" {
                state.screencopy_manager =
                    Some(registry.bind::<ZwlrScreencopyManagerV1, (), State>(
                        name,
                        version.min(3),
                        qh,
                        (),
                    ));
            } else if interface == "wl_shm" {
                state.shm = Some(registry.bind::<WlShm, (), State>(name, version, qh, ()));
            } else if interface == "wl_compositor" {
                state.compositor =
                    Some(registry.bind::<WlCompositor, (), State>(name, version.min(4), qh, ()));
            } else if interface == "zwlr_layer_shell_v1" {
                state.layer_shell = Some(registry.bind::<ZwlrLayerShellV1, (), State>(
                    name,
                    version.min(5),
                    qh,
                    (),
                ));
            } else if interface == "wl_seat" {
                state.seat = Some(registry.bind::<WlSeat, (), State>(name, version.min(8), qh, ()));
            }
        }
    }
}

impl Dispatch<ZxdgOutputManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZxdgOutputManagerV1,
        _: zxdg_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrScreencopyManagerV1,
        _: zwlr_screencopy_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShm, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShmPool, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
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

impl Dispatch<WlOutput, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &WlOutput,
        _event: wl_output::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
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

impl Dispatch<WlBuffer, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlBuffer,
        _: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
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

            // Mark either screenshot or dimming layer as configured
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

impl Dispatch<WlCompositor, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlCompositor,
        _: wl_compositor::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrLayerShellV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrLayerShellV1,
        _: zwlr_layer_shell_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSurface, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlSurface,
        _: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

fn main() -> Result<()> {
    let _cli = Cli::parse();

    let conn = Connection::connect_to_env().context("Failed to connect to Wayland")?;

    let mut event_queue = conn.new_event_queue();

    let mut state = State {
        outputs: Vec::new(),
        zxdg_output_manager: None,
        screencopy_manager: None,
        shm: None,
        compositor: None,
        layer_shell: None,
        seat: None,
        exit_requested: false,
        cursor: CursorPosition::default(),
        cursor_start: None,
        cursor_end: None,
        committed_mouse: None,
    };

    let qh = event_queue.handle();

    let _registry = conn.display().get_registry(&qh, ());
    event_queue.roundtrip(&mut state)?;

    let output_manager = state
        .zxdg_output_manager
        .as_ref()
        .ok_or(anyhow!("Could not obtain xdg output manager!"))?;

    let screencopy_manager = state
        .screencopy_manager
        .as_ref()
        .ok_or(anyhow!("Could not obtain screencopy manager!"))?;

    for (i, output) in state.outputs.iter_mut().enumerate() {
        output_manager.get_xdg_output(&output.proxy, &qh, i);
        output.frame = Some(screencopy_manager.capture_output(0, &output.proxy, &qh, i));
    }

    event_queue.roundtrip(&mut state)?;

    let shm = state.shm.as_ref().ok_or(anyhow!("Could not get shm!"))?;

    for output in state.outputs.iter_mut() {
        let buffer_info = output
            .buffer_info
            .as_ref()
            .ok_or(anyhow!("Did not recieve buffer information!"))?;
        let memfd = memfd_create(
            &CString::new("wayland-shm").unwrap(),
            MemFdCreateFlag::MFD_CLOEXEC,
        )?;
        let pool_size = buffer_info.stride * buffer_info.height;
        ftruncate(&memfd, pool_size as i64)?;
        let pool = shm.create_pool(memfd.as_fd(), pool_size as i32, &qh, ());
        let buffer = pool.create_buffer(
            0,
            buffer_info.width as i32,
            buffer_info.height as i32,
            buffer_info.stride as i32,
            buffer_info.format,
            &qh,
            (),
        );
        output.screenshot_memfd = Some(File::from(memfd));
        output.frame.as_mut().unwrap().copy(&buffer);
    }

    while !state.outputs.iter().all(|x| x.screenshot_captured) {
        event_queue.roundtrip(&mut state)?;
    }

    // Create layer surfaces
    for (i, output) in state.outputs.iter_mut().enumerate() {
        let compositor = state
            .compositor
            .as_ref()
            .ok_or(anyhow!("Could not obtain compositor!"))?;
        let layer_shell = state
            .layer_shell
            .as_ref()
            .ok_or(anyhow!("Could not obtain layer shell!"))?;

        // Create screenshot layer
        let screenshot_surface = compositor.create_surface(&qh, ());
        let screenshot_layer_surface = layer_shell.get_layer_surface(
            &screenshot_surface,
            Some(&output.proxy),
            Layer::Overlay,
            "simple-ss".to_string(),
            &qh,
            i,
        );
        screenshot_layer_surface
            .set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        screenshot_layer_surface.set_exclusive_zone(-1);
        screenshot_layer_surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        screenshot_surface.commit();

        output.screenshot_layer = Some(SurfaceLayer {
            surface: screenshot_surface,
            layer_surface: screenshot_layer_surface,
            configured: false,
        });

        // Create dimming layer
        let dimming_surface = compositor.create_surface(&qh, ());
        let dimming_layer_surface = layer_shell.get_layer_surface(
            &dimming_surface,
            Some(&output.proxy),
            Layer::Overlay,
            "simple-ss-dimming".to_string(),
            &qh,
            i,
        );
        dimming_layer_surface
            .set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        dimming_layer_surface.set_exclusive_zone(-1);
        dimming_layer_surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        dimming_surface.commit();

        output.dimming_layer = Some(SurfaceLayer {
            surface: dimming_surface,
            layer_surface: dimming_layer_surface,
            configured: false,
        });
    }

    while !state.outputs.iter().all(|x| {
        x.screenshot_layer.as_ref().is_some_and(|l| l.configured)
            && x.dimming_layer.as_ref().is_some_and(|l| l.configured)
    }) {
        event_queue.roundtrip(&mut state)?;
    }

    // Mark layers as configured after roundtrip
    for output in &mut state.outputs {
        if let Some(ref mut layer) = output.screenshot_layer {
            layer.configured = true;
        }
        if let Some(ref mut layer) = output.dimming_layer {
            layer.configured = true;
        }
    }

    // Create buffers for screenshot and dimming overlay
    for output in state.outputs.iter_mut() {
        let buffer_info = output.buffer_info.as_ref().unwrap();
        let shm = state.shm.as_ref().ok_or(anyhow!("Could not get shm!"))?;

        // Create screenshot buffer directly from capture memfd (no conversion!)
        // Xbgr8888 and Abgr8888 have identical RGB byte layout
        let pool_size = buffer_info.stride * buffer_info.height;
        let pool = shm.create_pool(
            output.screenshot_memfd.as_ref().unwrap().as_fd(),
            pool_size as i32,
            &qh,
            (),
        );
        let buffer = pool.create_buffer(
            0,
            buffer_info.width as i32,
            buffer_info.height as i32,
            buffer_info.stride as i32,
            Format::Abgr8888, // Works for Xbgr8888 - RGB bytes are identical
            &qh,
            (),
        );
        output.screenshot_buffer = Some(buffer);

        // Create dimming double buffer
        let dimming_layer = output.dimming_layer.as_ref().unwrap();
        let dimming_double_buffer =
            DimmingDoubleBuffer::new(shm, &qh, buffer_info, &dimming_layer.surface)?;

        output.dimming_double_buffer = Some(dimming_double_buffer);
    }

    // Attach buffers
    for output in state.outputs.iter_mut() {
        // Attach screenshot buffer
        let screenshot_layer = output.screenshot_layer.as_ref().unwrap();
        let buffer = output.screenshot_buffer.as_ref().unwrap();
        let buffer_info = output.buffer_info.as_ref().unwrap();

        screenshot_layer.surface.attach(Some(buffer), 0, 0);
        screenshot_layer.surface.damage_buffer(
            0,
            0,
            buffer_info.width as i32,
            buffer_info.height as i32,
        );
        screenshot_layer.surface.commit();
    }

    use std::time::{Duration, Instant};

    let start = Instant::now();
    let timeout = Duration::from_secs(5);

    while start.elapsed() < timeout {
        if state.exit_requested {
            break;
        }
        event_queue.blocking_dispatch(&mut state)?;
        event_queue.flush()?;
    }

    // Print selection results
    if let (Some(cursor_start), Some(cursor_end)) = (state.cursor_start, state.cursor_end) {
        eprintln!(
            "Selection: ({}, {}) -> ({}, {})",
            cursor_start.x, cursor_start.y, cursor_end.x, cursor_end.y
        );
    }

    event_queue.flush()?;

    Ok(())
}

// Seat event listener - get pointer and keyboard
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

// Pointer event listener - track cursor position
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
                ..
            } => {
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
                state.cursor = CursorPosition {
                    x: surface_x,
                    y: surface_y,
                };
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

// Keyboard event listener - handle Escape key
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
            // KEY_ESC = 1 (Linux input event codes)
            if key == 1 {
                if let WEnum::Value(wl_keyboard::KeyState::Pressed) = key_state {
                    state.exit_requested = true;
                }
            }
        }
    }
}
