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

    pub fn on_enter(&mut self, surface_id: ObjectId, x: f64, y: f64) {
        let offset = self
            .surface_offsets
            .get(&surface_id)
            .copied()
            .unwrap_or_default();
        self.current_surface = Some(surface_id);
        self.global_pos = Coordinate {
            x: x as i32,
            y: y as i32,
        } + offset;
    }

    pub fn on_motion(&mut self, x: f64, y: f64) {
        let surface_id = match &self.current_surface {
            Some(id) => id,
            None => return,
        };
        let offset = self
            .surface_offsets
            .get(surface_id)
            .copied()
            .unwrap_or_default();
        self.global_pos = Coordinate {
            x: x as i32,
            y: y as i32,
        } + offset;
    }

    pub fn on_button_press(&mut self) {
        self.start = Some(self.global_pos);
    }

    pub fn on_button_release(&mut self) {
        self.end = Some(self.global_pos);
    }

    pub fn selection(&self) -> Option<(Coordinate, Coordinate)> {
        Some((self.start?, self.end?))
    }

    pub fn is_selecting(&self) -> bool {
        self.start.is_some()
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
                app_state
                    .cursor
                    .on_enter(surface.id(), surface_x, surface_y);
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                app_state.cursor.on_motion(surface_x, surface_y);
                if app_state.cursor.is_selecting() {
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
                    app_state.cursor.on_button_press();
                }
                WEnum::Value(ButtonState::Released) => {
                    app_state.cursor.on_button_release();
                    app_state.exit_requested = true;
                }
                _ => {}
            },
            _ => {}
        }
    }
}
