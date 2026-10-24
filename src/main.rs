use anyhow::{anyhow, Context, Result};
use clap::Parser;
use memmap2::MmapMut;
use nix::sys::memfd::{memfd_create, MemFdCreateFlag};
use nix::unistd::ftruncate;
use std::ffi::CString;
use std::fs::File;
use std::io::Write;
use std::os::fd::AsFd;
use tiny_skia::{BlendMode, PixmapMut, PixmapPaint, PixmapRef, Transform};
use wayland_client::protocol::wl_shm::Format;
use wayland_client::Connection;

mod buffer;
mod state;

use buffer::{DimmingBuffer, SurfaceLayer};
use state::{CursorPosition, State};

#[derive(Parser)]
#[command(name = "simple-ss")]
#[command(about = "A minimal screenshot tool for Wayland", long_about = None)]
struct Cli {}

fn main() -> Result<()> {
    let _cli = Cli::parse();

    let conn = Connection::connect_to_env().context("Failed to connect to Wayland")?;

    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();
    conn.display().get_registry(&qh, ());

    let mut state = State::default();

    event_queue.roundtrip(&mut state)?;

    let output_manager = state.globals.zxdg_output_manager()?;
    let screencopy_manager = state.globals.screencopy_manager()?;

    for (i, output) in state.outputs.iter_mut().enumerate() {
        output_manager.get_xdg_output(&output.proxy, &qh, i);
        output.frame = Some(screencopy_manager.capture_output(0, &output.proxy, &qh, i));
    }

    event_queue.roundtrip(&mut state)?;

    let shm = state.globals.shm()?;

    for (i, output) in state.outputs.iter_mut().enumerate() {
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
            i,
        );
        output.screenshot_memfd = Some(File::from(memfd));
        output.frame.as_mut().unwrap().copy(&buffer);
    }

    while !state.outputs.iter().all(|x| x.screenshot_captured) {
        event_queue.roundtrip(&mut state)?;
    }

    // Create layer surfaces
    for (i, output) in state.outputs.iter_mut().enumerate() {
        let compositor = state.globals.compositor()?;
        let layer_shell = state.globals.layer_shell()?;

        output.screenshot_layer = Some(SurfaceLayer::new(
            compositor,
            layer_shell,
            &output.proxy,
            &qh,
            "simple-ss",
            i,
        ));

        output.dimming_layer = Some(SurfaceLayer::new(
            compositor,
            layer_shell,
            &output.proxy,
            &qh,
            "simple-ss-dimming",
            i,
        ));
    }

    while !state.outputs.iter().all(|x| {
        x.screenshot_layer.as_ref().is_some_and(|l| l.configured)
            && x.dimming_layer.as_ref().is_some_and(|l| l.configured)
    }) {
        event_queue.roundtrip(&mut state)?;
    }

    // Create buffers for screenshot and dimming overlay
    for (i, output) in state.outputs.iter_mut().enumerate() {
        let buffer_info = output.buffer_info.as_ref().unwrap();
        let shm = state.globals.shm()?;

        let pool = shm.create_pool(
            output.screenshot_memfd.as_ref().unwrap().as_fd(),
            (buffer_info.stride * buffer_info.height) as i32,
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
            i,
        );
        output.screenshot_buffer = Some(buffer);
        // Create dimming buffer
        let dimming_buffer = DimmingBuffer::new(shm, &qh, buffer_info, i)?;

        output.dimming_buffer = Some(dimming_buffer);
    }

    // Attach screenshot buffers
    for output in state.outputs.iter_mut() {
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

    // Initial render of dimming overlay (fully dimmed, no cutout)
    for (i, output) in state.outputs.iter_mut().enumerate() {
        if let (Some(ref mut dimming), Some(ref dimming_layer)) =
            (&mut output.dimming_buffer, &output.dimming_layer)
        {
            // Render with zero-size selection (fully dimmed)
            let pos = CursorPosition { x: 0.0, y: 0.0 };
            dimming.render(pos, pos)?;

            // Request frame callback and commit
            dimming.commit(&dimming_layer.surface, &qh, i);
        }
    }

    while !state.exit_requested {
        event_queue.blocking_dispatch(&mut state)?;
        event_queue.flush()?;

        // Update dimming buffers for each output
        for (i, output) in state.outputs.iter_mut().enumerate() {
            if let (Some(ref mut dimming), Some(ref dimming_layer)) =
                (&mut output.dimming_buffer, &output.dimming_layer)
            {
                let cursor_start = state.cursor_start.unwrap_or_default();

                dimming.update(
                    &dimming_layer.surface,
                    &qh,
                    i,
                    &output.info,
                    cursor_start,
                    state.cursor,
                )?;
            }
        }
    }

    event_queue.flush()?;

    if let (Some(start), Some(end)) = (state.cursor_start, state.cursor_end) {
        let sel_x = start.x.min(end.x) as i32;
        let sel_y = start.y.min(end.y) as i32;
        let sel_w = (start.x.max(end.x) - start.x.min(end.x)) as u32;
        let sel_h = (start.y.max(end.y) - start.y.min(end.y)) as u32;

        // Create destination buffer (zeroed = black transparent)
        let mut dest_data = vec![0u8; (sel_w * sel_h * 4) as usize];
        let mut dest_pixmap =
            PixmapMut::from_bytes(dest_data.as_mut_slice(), sel_w, sel_h).unwrap();

        // Draw each output's screenshot at its global position relative to selection
        for output in &state.outputs {
            let out_x = output.info.x;
            let out_y = output.info.y;
            let out_w = output.info.width as u32;
            let out_h = output.info.height as u32;

            // mmap the screenshot memfd
            let mmap = unsafe { MmapMut::map_mut(output.screenshot_memfd.as_ref().unwrap())? };

            // Create PixmapRef from the screenshot buffer
            if let Some(src_pixmap) = PixmapRef::from_bytes(mmap.as_ref(), out_w, out_h) {
                // Calculate offset: output position relative to selection top-left
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

        // Encode as PNG to stdout
        if let Ok(png_data) = dest_pixmap.as_ref().encode_png() {
            std::io::stdout().write_all(&png_data)?;
        }
    }

    Ok(())
}
