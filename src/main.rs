use anyhow::{anyhow, Context, Result};
use clap::Parser;
use nix::sys::memfd::{memfd_create, MemFdCreateFlag};
use nix::unistd::ftruncate;
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsFd;
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_surface};
use wayland_client::{
    protocol::{
        wl_output::{self, WlOutput},
        wl_registry,
        wl_shm::{self, Format},
        wl_shm_pool::WlShmPool,
    },
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::{
    self, ZxdgOutputManagerV1,
};
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1;
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1, zwlr_screencopy_manager_v1,
};

#[derive(Parser)]
#[command(name = "simple-ss")]
#[command(about = "A minimal screenshot tool for Wayland", long_about = None)]
struct Cli {}

struct Output {
    proxy: WlOutput,
    info: OutputInfo,
    buffer_info: Option<BufferInfo>,
    frame: Option<ZwlrScreencopyFrameV1>,
    screenshot_captured: bool,
    memfd: Option<File>,
    layer_surface: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    surface: Option<wl_surface::WlSurface>,
    layer_configured: bool,
    buffer_object: Option<wl_buffer::WlBuffer>,
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
            memfd: None,
            layer_surface: None,
            surface: None,
            layer_configured: false,
            buffer_object: None,
        }
    }
}

struct State {
    zxdg_output_manager: Option<ZxdgOutputManagerV1>,
    screencopy_manager: Option<zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1>,
    shm: Option<wl_shm::WlShm>,
    compositor: Option<wl_compositor::WlCompositor>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    outputs: Vec<Output>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
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
                let info = Output::from_proxy(registry.bind::<wl_output::WlOutput, (), State>(
                    name,
                    version,
                    qh,
                    (),
                ));
                state.outputs.push(info);
            } else if interface == "zxdg_output_manager_v1" {
                state.zxdg_output_manager = Some(registry.bind::<ZxdgOutputManagerV1, (), State>(
                    name,
                    version.min(2),
                    qh,
                    (),
                ));
            } else if interface == "zwlr_screencopy_manager_v1" {
                state.screencopy_manager = Some(
                    registry
                        .bind::<zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1, (), State>(
                            name,
                            version.min(3),
                            qh,
                            (),
                        ),
                );
            } else if interface == "wl_shm" {
                state.shm = Some(registry.bind::<wl_shm::WlShm, (), State>(name, version, qh, ()));
            } else if interface == "wl_compositor" {
                state.compositor = Some(registry.bind::<wl_compositor::WlCompositor, (), State>(
                    name,
                    version.min(4),
                    qh,
                    (),
                ));
            } else if interface == "zwlr_layer_shell_v1" {
                state.layer_shell = Some(
                    registry.bind::<zwlr_layer_shell_v1::ZwlrLayerShellV1, (), State>(
                        name,
                        version.min(5),
                        qh,
                        (),
                    ),
                );
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

impl Dispatch<zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
        _: zwlr_screencopy_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm::WlShm, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_shm::WlShm,
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
        _: wayland_client::protocol::wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1, usize> for State {
    fn event(
        state: &mut Self,
        _: &zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1,
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
                    format: format,
                    width: width,
                    height: height,
                    stride: stride,
                });
            }
            zwlr_screencopy_frame_v1::Event::Ready { .. } => {
                state.outputs[*output].screenshot_captured = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &wl_output::WlOutput,
        _event: <wl_output::WlOutput as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, usize> for State {
    fn event(
        state: &mut Self,
        _proxy: &zxdg_output_v1::ZxdgOutputV1,
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
            zxdg_output_v1::Event::Name { name } => {
                if !name.is_empty() {
                    info.name = name;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_buffer::WlBuffer,
        _: <wl_buffer::WlBuffer as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, usize> for State {
    fn event(
        state: &mut Self,
        layer_surface: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        output_idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = event {
            layer_surface.ack_configure(serial);
            state.outputs[*output_idx].layer_configured = true;
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_compositor::WlCompositor,
        _: wl_compositor::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_layer_shell_v1::ZwlrLayerShellV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &zwlr_layer_shell_v1::ZwlrLayerShellV1,
        _: zwlr_layer_shell_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_surface::WlSurface,
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
        output.memfd = Some(File::from(memfd));
        output.frame.as_mut().unwrap().copy(&buffer);
    }

    while !state.outputs.iter().all(|x| x.screenshot_captured) {
        event_queue.roundtrip(&mut state)?;
    }

    let compositor = state
        .compositor
        .as_ref()
        .ok_or(anyhow!("Could not obtain compositor!"))?;
    let layer_shell = state
        .layer_shell
        .as_ref()
        .ok_or(anyhow!("Could not obtain layer shell!"))?;

    for (i, output) in state.outputs.iter_mut().enumerate() {
        let surface = compositor.create_surface(&qh, ());

        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            Some(&output.proxy),
            zwlr_layer_shell_v1::Layer::Overlay,
            "simple-ss".to_string(),
            &qh,
            i,
        );

        layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer_surface.set_exclusive_zone(-1);
        layer_surface
            .set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);

        output.layer_surface = Some(layer_surface);

        surface.commit();
        output.surface = Some(surface);
    }

    while !state.outputs.iter().all(|x| x.layer_configured) {
        event_queue.roundtrip(&mut state)?;
    }

    for (_i, output) in state.outputs.iter_mut().enumerate() {
        let buffer_info = output.buffer_info.as_ref().unwrap();
        let width = buffer_info.width as usize;
        let height = buffer_info.height as usize;
        let stride = buffer_info.stride as usize;

        let memfd = output.memfd.as_mut().unwrap();
        memfd.seek(SeekFrom::Start(0))?;

        let mut data = Vec::new();
        memfd.read_to_end(&mut data)?;

        let mut rgba_data = Vec::with_capacity(width * height * 4);

        for row in 0..height {
            let row_start = row * stride;
            for col in 0..width {
                let pixel_start = row_start + col * 4;
                if pixel_start + 4 <= data.len() {
                    match buffer_info.format {
                        Format::Argb8888 => {
                            rgba_data.push(data[pixel_start + 1]);
                            rgba_data.push(data[pixel_start + 2]);
                            rgba_data.push(data[pixel_start + 3]);
                            rgba_data.push(data[pixel_start + 0]);
                        }
                        Format::Xrgb8888 => {
                            rgba_data.push(data[pixel_start + 1]);
                            rgba_data.push(data[pixel_start + 2]);
                            rgba_data.push(data[pixel_start + 3]);
                            rgba_data.push(255);
                        }
                        Format::Xbgr8888 => {
                            rgba_data.push(data[pixel_start + 0]);
                            rgba_data.push(data[pixel_start + 1]);
                            rgba_data.push(data[pixel_start + 2]);
                            rgba_data.push(255);
                        }
                        _ => {
                            return Err(anyhow!(
                                "Unsupported buffer format: {:?}",
                                buffer_info.format
                            ));
                        }
                    }
                }
            }
        }

        let display_memfd = memfd_create(
            &CString::new("wayland-shm").unwrap(),
            MemFdCreateFlag::MFD_CLOEXEC,
        )?;
        ftruncate(&display_memfd, (width * height * 4) as i64)?;

        let mut file = File::from(display_memfd);
        file.write_all(&rgba_data)?;
        file.flush()?;

        let shm = state.shm.as_ref().ok_or(anyhow!("Could not get shm!"))?;
        let pool = shm.create_pool(file.as_fd(), (width * height * 4) as i32, &qh, ());
        let buffer = pool.create_buffer(
            0,
            buffer_info.width as i32,
            buffer_info.height as i32,
            (width * 4) as i32,
            Format::Abgr8888,
            &qh,
            (),
        );

        output.buffer_object = Some(buffer);
    }

    for output in &mut state.outputs {
        let surface = output.surface.as_ref().unwrap();
        let buffer = output.buffer_object.as_ref().unwrap();
        let buffer_info = output.buffer_info.as_ref().unwrap();

        surface.attach(Some(buffer), 0, 0);
        surface.damage_buffer(0, 0, buffer_info.width as i32, buffer_info.height as i32);
        surface.commit();
    }

    use std::time::{Duration, Instant};

    let start = Instant::now();
    let timeout = Duration::from_secs(2);

    while start.elapsed() < timeout {
        event_queue.dispatch_pending(&mut state)?;
        event_queue.flush()?;
    }

    event_queue.flush()?;

    Ok(())
}
