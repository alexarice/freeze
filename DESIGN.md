# nix-shot - Design Document

A minimal Rust tool for region screenshots with screen freeze on Wayland.

## Goal

Replace Noctalia's screenshot functionality with a lightweight, standalone tool that:
1. Captures the screen
2. Displays it fullscreen (freeze effect)
3. Lets user select a region
4. Copies cropped region to clipboard and/or saves to file

## Scope (MVP)

**Essential:**
- Region selection with screen freeze overlay
- Clipboard copy (PNG)
- Save to `$XDG_PICTURES_DIR/Screenshots/`
- Single monitor (focused output)

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
| `wl_output` | Monitor enumeration | `wayland-client` |
| `wl_seat` | Pointer/keyboard input | `wayland-client` |
| `zwlr_layer_shell_v1` (v4+) | Fullscreen overlay surface | `wayland-protocols-wlr` |
| `zwlr_screencopy_manager_v1` (v3+) | Screen capture | `wayland-protocols-wlr` |

**Minimum protocol versions:**
- Sway 1.8+ provides all required protocols
- layer-shell-v1 for overlay positioning
- screencopy-v1 for framebuffer access

---

## Rust Dependencies

```toml
[dependencies]
# Wayland
wayland-client = "0.31"
wayland-protocols = "0.32"
wayland-protocols-wlr = "0.3"
wayland-backend = "0.3"

# Rendering (software, no OpenGL)
tiny-skia = "0.11"

# Image handling
image = "0.25"        # PNG encoding, cropping, format conversion
png = "0.17"          # PNG encoding (used by image)

# Clipboard
wl-clipboard-rs = "0.8"

# CLI
clap = { version = "4.4", features = ["derive"] }

# Utilities
thiserror = "1.0"     # Error types
xdg = "2.5"           # XDG directories
log = "0.4"           # Logging
env_logger = "0.11"   # Log backend
```

**Total:** ~10 core crates, all pure Rust or well-maintained bindings

---

## Control Flow

```
┌─────────────────────────────────────────────────────────────┐
│  main()                                                     │
│  ─────                                                      │
│  1. Parse CLI args                                          │
│  2. Initialize logger                                       │
│  3. Run screenshot pipeline                                 │
│  4. Handle errors, exit                                     │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│  ScreenshotPipeline::run()                                  │
│  ────────────────────────                                   │
│  1. wayland::connect() → WaylandState                       │
│  2. capture::screen(&wayland) → CapturedImage               │
│  3. overlay::show(image) → Option<SelectionRect>            │
│  4. export::run(image, rect, args) → Result<()>             │
└─────────────────────────────────────────────────────────────┘
                            │
            ┌───────────────┼───────────────┐
            ▼               ▼               ▼
    ┌──────────────┐ ┌──────────────┐ ┌──────────────┐
    │   capture/   │ │   overlay/   │ │   export/    │
    │   image.rs   │ │   selection.rs│ │   file.rs    │
    │              │ │              │ │   clipboard.rs│
    │ - screencopy │ │ - layer-shell│ │ - wl-copy    │
    │ - PNG encode │ │ - tiny-skia  │ │ - xdg save   │
    │ - BGR→RGBA   │ │ - input grab │ │ - crop       │
    └──────────────┘ └──────────────┘ └──────────────┘
```

### Detailed Flow

```rust
fn main() -> Result<()> {
    let args = Cli::parse();
    
    // Phase 1: Wayland setup
    let mut wayland = wayland::connect()?;
    wayland.roundtrip()?;  // Enumerate outputs, seat, etc.
    
    // Phase 2: Capture screen
    let output = wayland.focused_output()?;
    let image = capture::screen(&mut wayland, output)?;
    
    // Phase 3: Show overlay, get selection
    let selection = overlay::show(&mut wayland, &image)?;
    
    // Phase 4: Export
    if let Some(rect) = selection {
        let cropped = image.crop(rect);
        
        if args.clipboard {
            clipboard::copy_png(&cropped)?;
        }
        
        if args.save {
            file::save(&cropped, args.output_dir)?;
        }
    }
    
    Ok(())
}
```

---

## Module Responsibilities

### `wayland/`

**`mod.rs`** - Connection management
- `WaylandState`: Holds `Connection`, `Seat`, `Output` handles
- `connect()`: Initialize Wayland connection
- `roundtrip()`: Process events, enumerate globals
- `focused_output()`: Determine which monitor to capture

**`screencopy.rs`** - Screen capture
- `ScreencopyManager`: Wrapper around `zwlr_screencopy_manager_v1`
- `capture_frame(output)`: Request framebuffer, wait for completion
- Buffer format negotiation (XBGR vs ABGR)

**`layer.rs`** - Overlay surface
- `LayerSurface`: Wrapper around `zwlr_layer_surface_v1`
- `create_fullscreen()`: Create overlay covering entire output
- `set_exclusive()`: Grab keyboard/mouse input

### `capture/`

**`image.rs`** - Image buffer
- `CapturedImage`: RGBA buffer + dimensions + stride
- `from_screencopy()`: Convert wl_shm buffer to RGBA
- `crop(rect)`: Return new CapturedImage for region
- `to_png()`: Encode to PNG bytes

### `overlay/`

**`selection.rs`** - Selection UI
- `SelectionOverlay`: Holds layer surface, canvas, state
- `show(image)`: Display fullscreen, wait for input
- `draw()`: Render image + selection rectangle + dimensions
- `handle_input()`: Process pointer events

**State machine:**
```
Waiting → Dragging → Selected → Done
   ↑          ↑           ↑
   └─Escape───┴─Release───┘
```

**Input handling:**
- Button press: Start selection at cursor
- Motion: Update selection rectangle while dragging
- Button release: Finalize selection, return coordinates
- Escape: Cancel, return `None`

### `export/`

**`clipboard.rs`**
- `copy_png(image)`: Use `wl-clipboard-rs` to copy PNG data

**`file.rs`**
- `save(image, dir)`: Save to `$dir/YYYYMMDD_HHMMSS.png`
- Create directory if needed
- Generate unique filename

---

## Data Structures

### `CapturedImage`

```rust
pub struct CapturedImage {
    width: u32,
    height: u32,
    stride: u32,
    data: Vec<u8>,  // RGBA, row-major
}

impl CapturedImage {
    pub fn crop(&self, rect: SelectionRect) -> Self;
    pub fn to_png(&self) -> Result<Vec<u8>>;
}
```

### `SelectionRect`

```rust
pub struct SelectionRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}
```

### `WaylandState`

```rust
pub struct WaylandState {
    conn: Connection,
    seat: wl_seat::WlSeat,
    outputs: Vec<OutputInfo>,
    screencopy_manager: zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
    layer_shell: zwlr_layer_shell_v1::ZwlrLayerShellV1,
    compositor: wl_compositor::WlCompositor,
    shm: wl_shm::WlShm,
}
```

---

## Error Handling

All errors use `thiserror` for clean error types:

```rust
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Wayland connection failed: {0}")]
    Wayland(#[from] wayland_backend::client::BackendError),
    
    #[error("No outputs available")]
    NoOutputs,
    
    #[error("Screen capture failed: {0}")]
    Capture(String),
    
    #[error("Overlay creation failed: {0}")]
    Overlay(String),
    
    #[error("PNG encoding failed: {0}")]
    Png(#[from] png::EncodingError),
    
    #[error("Clipboard failed: {0}")]
    Clipboard(#[from] wl_clipboard_rs::Error),
    
    #[error("File I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
```

---

## CLI Interface

```bash
# Default: region → clipboard
nix-shot

# Save to file
nix-shot --save

# Both
nix-shot --save --clipboard

# Custom output directory
nix-shot --save --output ~/Screenshots

# Verbose
nix-shot -v
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

## Testing Strategy

### Unit Tests
- `CapturedImage::crop()` - Verify coordinates, dimensions
- `SelectionRect::contains()` - Point-in-rectangle tests
- File path generation - XDG compliance

### Integration Tests
- Clipboard copy/paste round-trip
- File save with various paths

### Manual Testing
- Sway compositor (primary target)
- Different monitor scaling factors
- Dark/light themes (overlay visibility)

---

## Implementation Phases

### Phase 1: Foundation (Week 1-2)
- [ ] Wayland connection
- [ ] Output enumeration
- [ ] Basic screencopy capture
- **Milestone:** `cargo run -- --test` saves full screenshot

### Phase 2: Overlay (Week 3-4)
- [ ] Layer-shell surface creation
- [ ] tiny-skia rendering
- [ ] Image display fullscreen
- **Milestone:** Overlay appears, blocks screen

### Phase 3: Selection (Week 5-6)
- [ ] Pointer input handling
- [ ] Selection rectangle drawing
- [ ] Coordinate return
- **Milestone:** Can select region, prints coordinates

### Phase 4: Export (Week 7)
- [ ] Image cropping
- [ ] Clipboard integration
- [ ] File saving
- **Milestone:** Full pipeline works

### Phase 5: Polish (Week 8)
- [ ] Error messages
- [ ] CLI help
- [ ] Documentation
- **Milestone:** `nix-shot --help` works, README written

---

## Repository Structure

```
nix-shot/
├── Cargo.toml
├── DESIGN.md           # This document
├── README.md           # User-facing documentation
├── LICENSE             # MIT
├── flake.nix           # NixOS development shell
├── src/
│   ├── main.rs         # CLI entry point
│   ├── error.rs        # Error types
│   ├── wayland/
│   │   ├── mod.rs      # Connection, state
│   │   ├── screencopy.rs
│   │   └── layer.rs
│   ├── capture/
│   │   └── image.rs
│   ├── overlay/
│   │   ├── mod.rs
│   │   └── selection.rs
│   └── export/
│       ├── mod.rs
│       ├── clipboard.rs
│       └── file.rs
└── tests/
    └── integration.rs
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

---

## Success Criteria

**MVP is complete when:**
1. `nix-shot` captures region with freeze effect
2. Clipboard copy works (can paste into other apps)
3. File save works (PNG in correct directory)
4. Escape cancels cleanly
5. Error messages are helpful

**Nice to have (post-MVP):**
- Dimensions badge during selection
- Capture sound
- Notification on save
- Config file for defaults

---

## License

MIT - same as grim, slurp, and most Wayland tooling
