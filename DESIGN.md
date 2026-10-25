# freeze - Design Document

A minimal Rust tool for region screenshots with screen freeze on Wayland.

## Goal

Replace Noctalia's screenshot functionality with a lightweight, standalone tool that:
1. Captures all monitors
2. Displays them fullscreen (freeze effect)
3. Lets user select a region
4. Copies cropped region to clipboard and/or saves to file

## Scope (MVP)

**Essential:**
- Region selection with screen freeze overlay
- Clipboard copy (PNG)
- Save to `$XDG_PICTURES_DIR/Screenshots/`
- Multi-monitor capture (all outputs)

**Out of scope (v2+):**
- Annotation
- Fullscreen capture mode
- Multi-monitor picker
- Delay timer

---

## Wayland Protocols

| Protocol | Purpose | Rust Crate |
|----------|---------|------------|
| `wl_compositor` (v4+) | Create surfaces | `wayland-client` |
| `wl_shm` | Shared memory buffers | `wayland-client` |
| `wl_output` (v3+) | Monitor enumeration with logical coordinates | `wayland-client` |
| `wl_seat` (v8+) | Pointer/keyboard input | `wayland-client` |
| `zwlr_layer_shell_v1` (v4+) | Fullscreen overlay surface | `wayland-protocols-wlr` |
| `zwlr_screencopy_manager_v1` (v3+) | Screen capture | `wayland-protocols-wlr` |
| `zxdg_output_manager_v1` (v2+) | Logical output coordinates | `wayland-protocols` |
| `wl_callback` | Frame synchronization | `wayland-client` |

**Minimum protocol versions:**
- Sway 1.8+ provides all required protocols
- layer-shell-v1 for overlay positioning
- screencopy-v1 for framebuffer access
- xdg-output-v1 for multi-monitor coordinate handling

---

## Rust Dependencies

```toml
[dependencies]
# Wayland
wayland-client = "0.31"
wayland-protocols = "0.32"
wayland-protocols-wlr = "0.3"
wayland-backend = "0.3"

# Image handling
image = "0.25"        # PNG encoding, cropping, format conversion
png = "0.17"          # PNG encoding (used by image)

# Clipboard
wl-clipboard-rs = "0.8"

# CLI
clap = { version = "4.4", features = ["derive"] }

# Utilities
anyhow = "1.0"        # Error handling
xdg = "2.5"           # XDG directories
log = "0.4"           # Logging
env_logger = "0.11"   # Log backend
nix = "0.29"          # memfd_create
memmap2 = "0.9"       # Memory-mapped I/O
```

**Rendering approach:** Manual pixel manipulation via mmap'd memfd buffers
- No external rendering library (tiny-skia/Cairo) for MVP
- Direct RGBA/ABGR byte manipulation
- Simpler dependency tree, full control

---

## Control Flow

```
┌─────────────────────────────────────────────────────────────┐
│  main()                                                     │
│  ─────                                                      │
│  1. Parse CLI args                                          │
│  2. Connect to Wayland                                      │
│  3. Enumerate outputs, bind globals                         │
│  4. Capture all screens via screencopy                      │
│  5. Create layer surfaces for each output                   │
│  6. Render initial dimming overlay, commit                  │
│  7. Event loop:                                             │
│     - Process input (mouse, keyboard)                       │
│     - Update selection rectangle                            │
│     - Render dimming + selection on frame callback          │
│  8. Export selection (clipboard/file)                       │
│  9. Exit                                                    │
└─────────────────────────────────────────────────────────────┘
```

### Main Loop

```rust
fn main() -> Result<()> {
    let args = Cli::parse();
    
    // Phase 1: Wayland setup
    let conn = Connection::connect_to_env()?;
    let mut event_queue = conn.new_event_queue();
    let mut state = State::new(&conn, event_queue.handle())?;
    
    // Phase 2: Capture screens
    state.capture_screens()?;
    
    // Phase 3: Show overlay (initial render)
    state.render_initial_dimming()?;
    
    // Phase 4: Event loop
    while state.running {
        event_queue.blocking_dispatch(&mut state)?;
        event_queue.flush()?;
        
        // State machine: render when buffer released + needs update
        state.try_render()?;
        
        // State machine: commit when callback received + rendered
        state.try_commit()?;
    }
    
    // Phase 5: Export
    if let Some(rect) = state.selection {
        export(&state.composite_image, rect, &args)?;
    }
    
    Ok(())
}
```

---

## Buffer Lifecycle & State Machine

### BufferState Enum

```rust
enum BufferState {
    Released,    // Memory available, waiting to render
    Rendered,    // Pixels written, waiting for callback
    Committed,   // With compositor, waiting for release
}
```

### State Transitions

```
┌──────────────┐
│  Released    │ ← wl_buffer::release
│  (can write) │
└──────┬───────┘
       │ if needs_render
       ▼
┌──────────────┐
│  Rendered    │ ← render_to_memfd()
│  (can commit) │
└──────┬───────┘
       │ if callback_received
       ▼
┌──────────────┐
│  Committed   │ ← attach() + commit()
│  (waiting)   │
└──────┬───────┘
       │ wl_buffer::release
       └──────→ back to Released
```

### Output State Tracking

```rust
struct Output {
    // Buffer lifecycle
    buffer_state: BufferState,
    callback_received: bool,
    needs_render: bool,
    
    // Resources
    surface: WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
    buffer: WlBuffer,
    memfd: File,
    
    // Metadata
    info: OutputInfo,
    width: i32,
    height: i32,
    stride: i32,
}
```

### Render Logic

```rust
// Called each loop iteration
fn try_render(&mut self) -> Result<()> {
    if self.buffer_state != BufferState::Released {
        return Ok(());
    }
    if !self.needs_render {
        return Ok(());
    }
    
    // Write pixels to memfd
    render_dimming_and_selection(&mut self.memfd, self.selection_rect);
    
    self.buffer_state = BufferState::Rendered;
    self.needs_render = false;
    Ok(())
}

fn try_commit(&mut self) -> Result<()> {
    if self.buffer_state != BufferState::Rendered {
        return Ok(());
    }
    if !self.callback_received {
        return Ok(());
    }
    
    // Attach and commit
    self.surface.attach(Some(&self.buffer), 0, 0);
    self.surface.damage_buffer(0, 0, self.width, self.height);
    self.surface.commit();
    
    self.buffer_state = BufferState::Committed;
    self.callback_received = false;
    Ok(())
}

// Event handlers
fn on_buffer_release(&mut self) {
    self.buffer_state = BufferState::Released;
}

fn on_frame_done(&mut self) {
    self.callback_received = true;
}

fn on_mouse_move(&mut self, pos: CursorPos) {
    self.cursor = pos;
    self.needs_render = true;
}
```

### Initial State

- After capturing screens and creating surfaces: **Render initial dimming → Committed**
- Buffer starts in `Committed` state after first render
- `needs_render = false` until mouse moves

---

## Input Handling

### Pointer Events

```rust
impl Dispatch<WlPointer, ()> for State {
    fn event(state, pointer, event, ...) {
        match event {
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                // Convert to global coordinates
                let output = state.current_output();
                state.cursor = CursorPosition {
                    x: surface_x + output.info.x as f64,
                    y: surface_y + output.info.y as f64,
                };
                state.needs_render = true;
            }
            wl_pointer::Event::Button { button, state: btn_state, .. } => {
                if button == BTN_LEFT && btn_state == Released {
                    // Finalize selection
                    state.selection = Some(calculate_rect(state.cursor_start, state.cursor));
                    state.running = false;
                } else if button == BTN_LEFT && btn_state == Pressed {
                    state.cursor_start = Some(state.cursor);
                }
            }
            _ => {}
        }
    }
}
```

### Keyboard Events

```rust
impl Dispatch<WlKeyboard, ()> for State {
    fn event(state, keyboard, event, ...) {
        if let wl_keyboard::Event::Key { key, state: Pressed, .. } = event {
            if key == KEY_ESC {
                state.selection = None;  // Cancel
                state.running = false;
            }
        }
    }
}
```

---

## Rendering Approach

### Pixel Format

- Using `ABGR8888` format (little-endian)
- Memory layout per pixel: `[B, G, R, A]`
- Row stride: `width * 4` bytes

### Dimming Overlay

```rust
// Fill entire buffer with semi-transparent black
for y in 0..height {
    for x in 0..width {
        let offset = (y * stride + x * 4) as usize;
        memfd[offset + 0] = 0;    // B
        memfd[offset + 1] = 0;    // G
        memfd[offset + 2] = 0;    // R
        memfd[offset + 3] = 128;  // A (50% transparent)
    }
}
```

### Selection Cutout

```rust
// Make selection area fully transparent (cutout)
let x1 = start.x.min(current.x) as i32;
let y1 = start.y.min(current.y) as i32;
let x2 = start.x.max(current.x) as i32;
let y2 = start.y.max(current.y) as i32;

for y in y1..y2 {
    for x in x1..x2 {
        let offset = (y * stride + x * 4) as usize;
        memfd[offset + 0] = 0;    // B
        memfd[offset + 1] = 0;    // G
        memfd[offset + 2] = 0;    // R
        memfd[offset + 3] = 0;    // A (fully transparent)
    }
}
```

### Multi-Monitor Handling

- Each output has its own buffer and state machine
- Selection rectangle stored in **global coordinates**
- When rendering per-output, convert to local:
  ```rust
  let local_x = global_x - output.info.x;
  let local_y = global_y - output.info.y;
  ```
- Only render if selection intersects that output

---

## Data Structures

### OutputInfo

```rust
pub struct OutputInfo {
    pub name: String,      // "DP-1", "HDMI-A-1", etc.
    pub x: i32,            // Global X coordinate (top-left corner)
    pub y: i32,            // Global Y coordinate (top-left corner)
    pub width: i32,        // Resolution width
    pub height: i32,       // Resolution height
    pub scale: i32,        // HiDPI scale factor (1, 2, etc.)
}
```

### CursorPosition

```rust
#[derive(Debug, Clone, Copy, Default)]
struct CursorPosition {
    x: f64,
    y: f64,
}
```

### SelectionRect

```rust
pub struct SelectionRect {
    pub x: i32,       // Global X
    pub y: i32,       // Global Y
    pub width: i32,
    pub height: i32,
}
```

---

## Error Handling

All errors use `anyhow` for simple, application-level error handling:

```rust
use anyhow::Result;

fn main() -> Result<()> {
    // Use ? operator for automatic error propagation
    let conn = Connection::connect_to_env()?;
    
    // Add context to errors
    let output = get_output()
        .context("Failed to get focused output")?;
    
    Ok(())
}
```

---

## CLI Interface

```bash
# Default: region → clipboard
freeze

# Save to file
freeze --save

# Both
freeze --save --clipboard

# Custom output directory
freeze --save --output ~/Screenshots

# Verbose
freeze -v
```

```rust
#[derive(clap::Parser)]
struct Cli {
    /// Save to file instead of clipboard
    #[arg(short, long)]
    save: bool,

    /// Copy to clipboard (default: true if --save not specified)
    #[arg(long)]
    clipboard: bool,

    /// Output directory for saved screenshots
    #[arg(short, long, default_value = "~/Pictures/Screenshots")]
    output: String,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,
}
```

---

## Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| wlr-screencopy quirks | Medium | High | Test on Sway early |
| Layer-shell positioning | Low | High | Use `exclusive_zone = -1` |
| Buffer format conversion | Medium | Medium | Test XBGR/ABGR both |
| Input grab conflicts | Low | Medium | Set `keyboard_interactivity = exclusive` |
| Clipboard format issues | Low | Low | wl-clipboard-rs is mature |
| Frame callback timing | Low | Medium | Wait for both release + callback |

---

## Success Criteria

**MVP is complete when:**
1. `freeze` captures region with freeze effect
2. Clipboard copy works (can paste into other apps)
3. File save works (PNG in correct directory)
4. Escape cancels cleanly
5. Error messages are helpful
6. Selection rectangle renders smoothly during drag

**Nice to have (post-MVP):**
- Dimensions badge during selection
- Border around selection rectangle
- Capture sound
- Notification on save
- Config file for defaults

---

## License

MIT - same as grim, slurp, and most Wayland tooling
