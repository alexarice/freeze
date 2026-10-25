use std::collections::HashMap;
use std::ops::{Add, Sub};
use wayland_client::backend::ObjectId;
use wayland_client::protocol::wl_pointer::{self, ButtonState, WlPointer};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};

use crate::app::App;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Coordinate {
    pub x: i32,
    pub y: i32,
}

impl Add for Coordinate {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            x: self.x + other.x,
            y: self.y + other.y,
        }
    }
}

impl Sub for Coordinate {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self {
            x: self.x - other.x,
            y: self.y - other.y,
        }
    }
}

#[derive(Default)]
pub struct CursorState {
    /// Maps surface ID → global offset (x, y)
    surface_offsets: HashMap<ObjectId, Coordinate>,
    /// Current surface ID the cursor is on
    current_surface: Option<ObjectId>,
    /// Current cursor position in global coordinates
    global_pos: Coordinate,
    /// Selection start (global coords)
    start: Option<Coordinate>,
    /// Selection end (global coords)
    end: Option<Coordinate>,
}

impl CursorState {
    pub fn register_surface(&mut self, surface_id: ObjectId, offset: Coordinate) {
        self.surface_offsets.insert(surface_id, offset);
    }

    pub fn selection(&self) -> Option<(Coordinate, Coordinate)> {
        Some((self.start?, self.end?))
    }

    pub fn global_pos(&self) -> Coordinate {
        self.global_pos
    }

    pub fn start(&self) -> Option<Coordinate> {
        self.start
    }
}

impl Dispatch<WlPointer, (), App> for CursorState {
    fn event(
        app_state: &mut App,
        _pointer: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                serial: _,
                surface,
            } => {
                let surface_id = surface.id();
                let offset = app_state
                    .cursor
                    .surface_offsets
                    .get(&surface_id)
                    .copied()
                    .unwrap_or_default();
                app_state.cursor.current_surface = Some(surface_id);
                app_state.cursor.global_pos = Coordinate {
                    x: surface_x as i32,
                    y: surface_y as i32,
                } + offset;
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                if let Some(surface_id) = &app_state.cursor.current_surface {
                    let offset = app_state
                        .cursor
                        .surface_offsets
                        .get(surface_id)
                        .copied()
                        .unwrap_or_default();
                    app_state.cursor.global_pos = Coordinate {
                        x: surface_x as i32,
                        y: surface_y as i32,
                    } + offset;
                }
                if app_state.cursor.start().is_some() {
                    for output in &mut app_state.outputs {
                        if let Some(ref mut dimming) = output.dimming_buffer {
                            dimming.needs_render = true;
                        }
                    }
                }
            }
            wl_pointer::Event::Button {
                state: btn_state, ..
            } => match btn_state {
                WEnum::Value(ButtonState::Pressed) => {
                    app_state.cursor.start = Some(app_state.cursor.global_pos);
                }
                WEnum::Value(ButtonState::Released) => {
                    app_state.cursor.end = Some(app_state.cursor.global_pos);
                    app_state.exit_requested = true;
                }
                _ => {}
            },
            _ => {}
        }
    }
}
