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

Keys: every key is pressed twice, once to see what it selects and once
to confirm it, so nothing is clicked by a single keystroke (`Enter`
confirms too). A hint's keys are where its target is: the keyboard is
laid over the window, so something in the bottom-left corner is
labelled `z` and something in the top-right `p`. `Space` switches
between hints and the letter grid, `Tab` picks another window with
`1`-`9`, `Backspace` undoes one press, `Esc` backs out and then quits.

Element hints need AT-SPI accessibility enabled on the system; Chromium
additionally needs a launch flag. See
[docs/how-it-works.md](docs/how-it-works.md) for setup, internals,
debug flags, and current limitations.
