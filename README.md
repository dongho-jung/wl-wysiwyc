# wl-wysiwyc

What You See Is What You Click for Wayland. Run it and the focused
window is hinted right away: windows that expose an accessibility tree
get vimium-style hint labels on their clickable elements, and
everything else falls back to a qwerty letter grid. No mouse needed.

Currently supports Hyprland.

## Install and run

```
cargo build --release
./target/release/wl-wysiwyc
```

Keys: in hint mode, type a label to click that element; in grid mode,
one letter clicks that qwerty tile. `Space` switches between the two,
`Tab` picks another window with `1`-`9`, `Backspace` edits a typed
hint, `Esc` goes back or quits.

Element hints need AT-SPI accessibility enabled on the system; Chromium
additionally needs a launch flag. See
[docs/how-it-works.md](docs/how-it-works.md) for setup, internals,
debug flags, and current limitations.
