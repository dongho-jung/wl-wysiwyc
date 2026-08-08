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

Keys: type a hint and the pointer goes to that target, then `-` clicks
it and `=` right-clicks it; typing another hint moves the target
instead. A hint's keys are where its target is: the keyboard is laid
over the window, so something in the bottom-left corner is labelled `z`
and something in the top-right `p`. `Space` switches between hints and
the letter grid, `Tab` picks another window with `1`-`9`, `Backspace`
undoes one press, `Esc` backs out and then quits. `Ctrl+Esc` gets the
keyboard back if a run is killed before it can tidy up.

Colours, label sizing, the confirm delay, and the limits on reading a
window are set in `~/.config/wl-wysiwyc/config.yaml`, which is optional:
see [docs/configuration.md](docs/configuration.md).

Element hints need AT-SPI accessibility enabled on the system; Chromium
additionally needs a launch flag. See
[docs/how-it-works.md](docs/how-it-works.md) for setup, internals,
debug flags, and current limitations.
