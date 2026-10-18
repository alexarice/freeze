use anyhow::{anyhow, Context, Result};
use clap::Parser;
use nix::sys::memfd::{memfd_create, MemFdCreateFlag};
use nix::unistd::ftruncate;
use wayland_client::protocol::wl_buffer;
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1;
use std::ffi::CString;
use std::os::fd::AsFd;
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
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1, zwlr_screencopy_manager_v1,
};

#[derive(Parser)]
#[command(name = "simple-ss")]
#[command(about = "A minimal screenshot tool for Wayland", long_about = None)]
struct Cli {}

struct Output {
    proxy: WlOutput,
    frame: Option<ZwlrScreencopyFrameV1>,
    info: OutputInfo,
    buffer: Option<BufferInfo>,
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
            buffer: Default::default(),
        }
    }
}

struct State {
    zxdg_output_manager: Option<ZxdgOutputManagerV1>,
    screencopy_manager: Option<zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1>,
    shm: Option<wl_shm::WlShm>,
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
        frame: &zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        output: &usize,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format: WEnum::Value(format),
                width,
                height,
                stride,
            } => {
                state.outputs[*output].buffer = Some(BufferInfo {
                    format: format,
                    width: width,
                    height: height,
                    stride: stride,
                });
            }
            _ => {
		println!("Other frame event {event:?}")
	    }
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


fn main() -> Result<()> {
    let _cli = Cli::parse();

    let conn = Connection::connect_to_env().context("Failed to connect to Wayland")?;

    let mut event_queue = conn.new_event_queue();

    let mut state = State {
        outputs: Vec::new(),
        zxdg_output_manager: None,
        screencopy_manager: None,
        shm: None,
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

    for (i, output) in state.outputs.iter_mut().enumerate() {
        let buffer_info = output
            .buffer
            .as_ref()
            .ok_or(anyhow!("Did not recieve buffer information!"))?;
        let memfd = memfd_create(
            &CString::new("wayland-shm").unwrap(),
            MemFdCreateFlag::MFD_CLOEXEC,
        )?;
        let pool_size = buffer_info.stride * buffer_info.height;
        ftruncate(&memfd, pool_size as i64)?;
        let pool =
            shm.create_pool(memfd.as_fd(), pool_size as i32, &qh, ());
        let buffer = pool.create_buffer(
            0,
            buffer_info.width as i32,
            buffer_info.height as i32,
            buffer_info.stride as i32,
            buffer_info.format,
            &qh,
            (),
        );
	output.frame.as_mut().unwrap().copy(&buffer)
    }

    event_queue.roundtrip(&mut state)?;
    event_queue.roundtrip(&mut state)?;

    println!("Found {} output(s)", state.outputs.len());
    for info in state.outputs.iter().map(|x| &x.info) {
        println!(
            "  {} at ({}, {}) size {}x{}",
            info.name, info.x, info.y, info.width, info.height
        );
    }

    Ok(())
}
