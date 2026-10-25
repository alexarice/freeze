use anyhow::{Context, Result};
use memmap2::MmapMut;
use nix::{
    sys::memfd::{memfd_create, MemFdCreateFlag},
    unistd::ftruncate,
};
use std::{ffi::CString, fs::File, os::fd::AsFd};
use tiny_skia::{BlendMode, Color, Paint, PixmapMut, Rect, Transform};
use wayland_client::{
    protocol::{
        wl_buffer::{self, WlBuffer},
        wl_callback::{self, WlCallback},
        wl_compositor::WlCompositor,
        wl_output::WlOutput,
        wl_shm::{Format, WlShm},
        wl_surface::WlSurface,
    },
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity},
};

use crate::app::{App, OutputInfo};
use crate::cursor::Coordinate;

/// Create a Wayland buffer from an existing memfd
pub fn create_buffer_from_memfd(
    shm: &WlShm,
    memfd: impl AsFd,
    info: &OutputInfo,
    qh: &QueueHandle<App>,
    user_data: usize,
    format: Option<Format>,
) -> Result<WlBuffer> {
    let width = info.width as i32;
    let height = info.height as i32;
    let stride = info.stride as i32;
    let format = format.unwrap_or(info.format);
    let pool_size = (info.stride * info.height) as i32;

    let pool = shm.create_pool(memfd.as_fd(), pool_size, qh, ());
    let buffer = pool.create_buffer(0, width, height, stride, format, qh, user_data);

    Ok(buffer)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BufferState {
    Released,  // Memory available, waiting to render
    Rendered,  // Pixels written, waiting for callback
    Committed, // With compositor, waiting for release
}

pub struct SurfaceLayer {
    pub surface: WlSurface,
}

impl SurfaceLayer {
    pub fn new(
        compositor: &WlCompositor,
        layer_shell: &ZwlrLayerShellV1,
        output: &WlOutput,
        qh: &QueueHandle<App>,
        name: &str,
        _output_idx: usize,
    ) -> Self {
        let surface = compositor.create_surface(qh, ());
        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            Some(output),
            Layer::Overlay,
            name.to_string(),
            qh,
            (),
        );
        layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        surface.commit();

        SurfaceLayer { surface }
    }

    pub fn attach_buffer(&self, buffer: &WlBuffer, width: i32, height: i32) {
        self.surface.attach(Some(buffer), 0, 0);
        self.surface.damage_buffer(0, 0, width, height);
        self.surface.commit();
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
        qh: &QueueHandle<App>,
        info: &OutputInfo,
        output_idx: usize,
    ) -> Result<Self> {
        let memfd = memfd_create(
            &CString::new("wayland-shm-dimming")?,
            MemFdCreateFlag::MFD_CLOEXEC,
        )?;
        let buffer_size = (info.stride * info.height) as i64;
        ftruncate(&memfd, buffer_size)?;

        let buffer =
            create_buffer_from_memfd(shm, &memfd, info, qh, output_idx, Some(Format::Abgr8888))?;

        Ok(DimmingBuffer {
            memfd: memfd.into(),
            buffer,
            state: BufferState::Released,
            needs_render: false,
            frame_callback: None,
            width: info.width as i32,
            height: info.height as i32,
        })
    }

    pub fn render(&mut self, start: Coordinate, current: Coordinate) -> Result<()> {
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
            clear_paint.blend_mode = BlendMode::Source;
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

    pub fn commit(&mut self, layer: &SurfaceLayer, qh: &QueueHandle<App>, output_idx: usize) {
        self.frame_callback = Some(layer.surface.frame(qh, output_idx));
        layer.attach_buffer(&self.buffer, self.width, self.height);
        self.state = BufferState::Committed;
    }

    pub fn update(
        &mut self,
        layer: &SurfaceLayer,
        qh: &QueueHandle<App>,
        output_idx: usize,
        output_info: &OutputInfo,
        cursor_start: Coordinate,
        cursor_current: Coordinate,
    ) -> Result<()> {
        // Try to render if needed
        if self.state == BufferState::Released && self.needs_render {
            // Convert global cursor to output-local coordinates
            let offset = Coordinate {
                x: output_info.x,
                y: output_info.y,
            };
            let local_start = cursor_start - offset;
            let local_current = cursor_current - offset;

            self.render(local_start, local_current)?;
        }

        // Commit if rendered and callback has fired
        if self.state == BufferState::Rendered && self.frame_callback.is_none() {
            self.commit(layer, qh, output_idx);
        }

        Ok(())
    }
}

impl Dispatch<WlBuffer, usize, App> for DimmingBuffer {
    fn event(
        app_state: &mut App,
        _buffer: &WlBuffer,
        event: wl_buffer::Event,
        output_idx: &usize,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let wl_buffer::Event::Release = event {
            if let Some(output) = app_state.outputs.get_mut(*output_idx) {
                if let Some(ref mut dimming) = output.dimming_buffer {
                    dimming.state = BufferState::Released;
                }
            }
        }
    }
}

impl Dispatch<WlCallback, usize, App> for DimmingBuffer {
    fn event(
        app_state: &mut App,
        _callback: &WlCallback,
        event: wl_callback::Event,
        output_idx: &usize,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            if let Some(output) = app_state.outputs.get_mut(*output_idx) {
                if let Some(ref mut dimming) = output.dimming_buffer {
                    dimming.frame_callback = None;
                }
            }
        }
    }
}
