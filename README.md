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

Keys: type a hint to arm it, then press the same key again to click it
(`Enter` works too), so nothing is clicked by a single keystroke. Hint
keys follow the keyboard layout: a target in the top-left corner is
labelled near `q`, one in the bottom-right near `m`. `Space` switches
between hints and the letter grid, `Tab` picks another window with
`1`-`9`, `Backspace` steps back, `Esc` backs out and then quits.

Element hints need AT-SPI accessibility enabled on the system; Chromium
additionally needs a launch flag. See
[docs/how-it-works.md](docs/how-it-works.md) for setup, internals,
debug flags, and current limitations.
