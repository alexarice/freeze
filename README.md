# freeze

A minimal Wayland screenshot tool with region selection, screen freeze overlay, and multi-monitor support.

## What it does

1. Captures all monitors
2. Freezes the screen with a dimming overlay
3. Lets you select a region with your mouse
4. Outputs the cropped screenshot as PNG to stdout

## How it works

freeze uses Wayland protocols to capture and display your screen:

- **zwlr_screencopy_manager_v1** - Captures framebuffer content from each output
- **zwlr_layer_shell_v1** - Creates fullscreen overlay surfaces above all other windows
- **zxdg_output_manager_v1** - Gets logical coordinates for multi-monitor setups

The tool creates two layer surfaces per monitor: one for the screenshot buffer and one for the interactive dimming overlay. The overlay uses semi-transparent black with a transparent cutout rectangle that follows your cursor, creating the selection effect.

## Installation

```bash
# From source
cargo install --path .

# Or build manually
git clone https://github.com/yourusername/freeze.git
cd freeze
cargo build --release
```

## Usage

```bash
# Select region, output PNG to stdout
freeze > screenshot.png

# Pipe to wl-copy for clipboard
freeze | wl-copy -t image/png

# Pipe to grim's slurp-style workflow
freeze | swappy -f -
```

Press **Escape** to cancel, **left click** to confirm selection.

## Requirements

- Wayland compositor with:
  - `wlr-screencopy-unstable-v1` protocol
  - `wlr-layer-shell-unstable-v1` protocol
  - `xdg-output-unstable-v1` protocol
- Sway 1.8+ or compatible compositor

## Building

```bash
cargo build --release
```

## License

MIT
