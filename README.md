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

Keys: the overlay opens with the target on whatever the pointer is
nearest, the arrow keys move it, and typing a hint sends it there. `-`
and `Enter` click the target and `=` right-clicks it; `keys.left_click`
takes a list, so give it as many keys as you like. Hold a click key
instead of tapping it to double-click, hold longer to triple-click, and
hold `Shift`, `Ctrl` or `Alt` with it to have the window see those too.
A hint's keys are where its target is: the keyboard is laid over the
window, so on qwerty something in the bottom-left corner is labelled `z`
and something in the top-right `p`. Set `keys.layout` for dvorak, or for
no layout at all, and `keys.excluded` to keep letters you would rather
not reach for out of the labels. `;` and `'` scroll the window under the
pointer without leaving the overlay, with `Shift` for the end of it and
`Ctrl` for sideways. `Space` switches between hints and the letter grid
unless you have given it another job, `Tab` picks another window with
`1`-`9`, `Backspace` undoes one press, and `Esc` backs out and then
quits. Set `keys.reset` to the key that opens the overlay: pressing it
starts the choices over, and pressing it with nothing to undo closes the
overlay. Moving the mouse closes it too, since reaching for it says the
keyboard was not what you wanted. `Ctrl+Esc` gets the keyboard back if a
run is killed before it can tidy up.

Colours, label sizing, which keys click, and the limits on reading a
window are set in `~/.config/wl-wysiwyc/config.yaml`, which is optional:
see [docs/configuration.md](docs/configuration.md).

Element hints need AT-SPI accessibility enabled on the system; Chromium
additionally needs a launch flag. See
[docs/how-it-works.md](docs/how-it-works.md) for setup, internals,
debug flags, and current limitations.
