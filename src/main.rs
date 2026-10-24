use anyhow::{anyhow, Context, Result};
use clap::Parser;
use memmap2::MmapMut;
use nix::sys::memfd::{memfd_create, MemFdCreateFlag};
use nix::unistd::ftruncate;
use std::ffi::CString;
use std::fs::File;
use std::os::fd::AsFd;
use tiny_skia::{BlendMode, Color, Paint, PixmapMut, PixmapPaint, PixmapRef, Rect, Transform};

use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_callback::WlCallback;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_shm::{Format, WlShm};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, QueueHandle};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{
    Layer, ZwlrLayerShellV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::KeyboardInteractivity;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::ZwlrLayerSurfaceV1;

mod state;

use state::{BufferInfo, CursorPosition, OutputInfo, State};

#[derive(Parser)]
#[command(name = "simple-ss")]
#[command(about = "A minimal screenshot tool for Wayland", long_about = None)]
struct Cli {}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BufferState {
    Released,  // Memory available, waiting to render
    Rendered,  // Pixels written, waiting for callback
    Committed, // With compositor, waiting for release
}

pub struct SurfaceLayer {
    pub surface: WlSurface,
    pub layer_surface: ZwlrLayerSurfaceV1,
    pub configured: bool,
}

impl SurfaceLayer {
    pub fn new(
        compositor: &WlCompositor,
        layer_shell: &ZwlrLayerShellV1,
        output: &WlOutput,
        qh: &QueueHandle<State>,
        name: &str,
        output_idx: usize,
    ) -> Self {
        let surface = compositor.create_surface(qh, ());
        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            Some(output),
            Layer::Overlay,
            name.to_string(),
            qh,
            output_idx,
        );
        layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        surface.commit();

        SurfaceLayer {
            surface,
            layer_surface,
            configured: false,
        }
    }
}

pub struct DimmingBuffer {
    pub memfd: File,
    pub buffer: WlBuffer,
    pub state: BufferState,
    pub needs_render: bool,
    pub frame_callback: Option<WlCallback>,
    pub width: i32,
    pub height: i32,
}

impl DimmingBuffer {
    pub fn new(
        shm: &WlShm,
        qh: &QueueHandle<State>,
        buffer_info: &BufferInfo,
        output_idx: usize,
    ) -> Result<Self> {
        let width = buffer_info.width as i32;
        let height = buffer_info.height as i32;
        let stride = buffer_info.stride as i32;
        let format = Format::Abgr8888;

        let memfd = memfd_create(
            &CString::new("wayland-shm-dimming")?,
            MemFdCreateFlag::MFD_CLOEXEC,
        )?;
        let buffer_size = (width * stride) as i64;
        ftruncate(&memfd, buffer_size)?;

        // Create buffer pool
        let pool = shm.create_pool(memfd.as_fd(), buffer_size as i32, qh, ());

        // Create buffer with output index for release tracking
        let buffer = pool.create_buffer(0, width, height, stride, format, qh, output_idx);

        Ok(DimmingBuffer {
            memfd: memfd.into(),
            buffer,
            state: BufferState::Released,
            needs_render: false,
            frame_callback: None,
            width,
            height,
        })
    }

    pub fn render(&mut self, start: CursorPosition, current: CursorPosition) -> Result<()> {
        let width = self.width as u32;
        let height = self.height as u32;

        let mut mmap = unsafe { MmapMut::map_mut(&self.memfd)? };

        // Create pixmap from mmap'd memory
        let mut pixmap = PixmapMut::from_bytes(mmap.as_mut(), width, height)
            .context("Failed to create pixmap")?;

        // Fill with semi-transparent black (dimming overlay)
        let dim_color = Color::from_rgba8(0, 0, 0, 128);
        pixmap.fill(dim_color);

        // Calculate cutout rectangle
        let x1 = start.x.min(current.x) as f32;
        let y1 = start.y.min(current.y) as f32;
        let x2 = start.x.max(current.x) as f32;
        let y2 = start.y.max(current.y) as f32;

        // Make cutout area fully transparent by clearing it
        if let Some(rect) = Rect::from_xywh(x1, y1, x2 - x1, y2 - y1) {
            let mut clear_paint = Paint::default();
            clear_paint.set_color_rgba8(0, 0, 0, 0);
            clear_paint.blend_mode = tiny_skia::BlendMode::Source;
            pixmap.fill_rect(rect, &clear_paint, Transform::identity(), None);
        }

        // Draw white border (2 pixels wide)
        let mut border_paint = Paint::default();
        border_paint.set_color_rgba8(255, 255, 255, 255);
        border_paint.anti_alias = true;

        // Draw border as 4 rectangles (top, bottom, left, right)
        let border_thickness = 2.0;
        if let Some(top_rect) = Rect::from_xywh(x1, y1, x2 - x1, border_thickness) {
            pixmap.fill_rect(top_rect, &border_paint, Transform::identity(), None);
        }
        if let Some(bottom_rect) =
            Rect::from_xywh(x1, y2 - border_thickness, x2 - x1, border_thickness)
        {
            pixmap.fill_rect(bottom_rect, &border_paint, Transform::identity(), None);
        }
        if let Some(left_rect) = Rect::from_xywh(x1, y1, border_thickness, y2 - y1) {
            pixmap.fill_rect(left_rect, &border_paint, Transform::identity(), None);
        }
        if let Some(right_rect) =
            Rect::from_xywh(x2 - border_thickness, y1, border_thickness, y2 - y1)
        {
            pixmap.fill_rect(right_rect, &border_paint, Transform::identity(), None);
        }

        self.state = BufferState::Rendered;
        self.needs_render = false;

        Ok(())
    }

    pub fn commit(&mut self, surface: &WlSurface, qh: &QueueHandle<State>, output_idx: usize) {
        // Request frame callback BEFORE commit and store it directly
        self.frame_callback = Some(surface.frame(qh, output_idx));

        surface.attach(Some(&self.buffer), 0, 0);
        surface.damage_buffer(0, 0, self.width, self.height);
        surface.commit();

        self.state = BufferState::Committed;
    }

    pub fn update(
        &mut self,
        surface: &WlSurface,
        qh: &QueueHandle<State>,
        output_idx: usize,
        output_info: &OutputInfo,
        cursor_start: CursorPosition,
        cursor_current: CursorPosition,
    ) -> Result<()> {
        // Try to render if needed
        if self.state == BufferState::Released && self.needs_render {
            // Convert global cursor to output-local coordinates
            let local_start = CursorPosition {
                x: cursor_start.x - output_info.x as f64,
                y: cursor_start.y - output_info.y as f64,
            };
            let local_current = CursorPosition {
                x: cursor_current.x - output_info.x as f64,
                y: cursor_current.y - output_info.y as f64,
            };

            self.render(local_start, local_current)?;
        }

        // Commit if rendered and callback has fired
        if self.state == BufferState::Rendered && self.frame_callback.is_none() {
            self.commit(surface, qh, output_idx);
        }

        Ok(())
    }
}

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
            use std::io::Write;
            std::io::stdout().write_all(&png_data)?;
        }
    }

    Ok(())
}
