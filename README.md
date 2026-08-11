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
nearest. Arrow keys accelerate it freely, two at once move diagonally,
and releasing one keeps the other direction held. Even the shortest press
gets a small initial nudge, then acceleration ramps gently for control
between dense anchors. The anchor it leaves is briefly excluded so a tap is
not pulled straight back. Anchors behind the pressed direction are excluded
for the same brief grace period, so a nearby anchor behind cannot beat a
distant one ahead. Releasing every arrow keeps the current inertia, then a
distance-sensitive spring attracts the pointer to the anchor nearest its
projected coast endpoint regardless of distance. Entering the small
snap zone places it exactly on that anchor. `Shift+arrow` jumps instantly to
the last anchor reached in that direction. `Alt+arrow` moves at a constant
speed with no acceleration, inertia, attraction, or snap, so it can stop at an
arbitrary coordinate. `Ctrl+arrow` jumps instantly to the next anchor in that
direction. Pushing against a window edge scrolls the window on the same axis.
Typing a hint
sends the pointer straight to its target. During keyboard navigation the hint
labels give way to small red anchor dots from a pre-rendered frame. The nearest
one is blue during free motion. A small focus ring moves independently, so
repainting the screen cannot stall the pointer. `-` and `Enter` click and
`=` right-clicks. Pressing a click key holds that mouse button down immediately,
and releasing the key releases the button and closes the overlay. Move with
arrows or type another label while it is held to drag there. `keys.left_click`
takes a list, so give it as many keys as you like, and `Shift`, `Ctrl` or `Alt`
with a click key reaches the window too. A hint's keys are where its target is.
When collision avoidance moves a label farther away, an outlined two-tone
dotted connector identifies its target across light and dark content. The
keyboard is laid over the window, so on qwerty
something in the bottom-left corner is labelled `z` and something in the
top-right `p`. Set `keys.layout` for dvorak, or for no layout at all,
and `keys.excluded` to keep letters you would rather not reach for out
of the labels. `;` and `'` scroll the window under the pointer without
leaving the overlay, with `Shift` for the end of it and `Ctrl` for
sideways. `Space` switches between hints and the letter grid unless you
have given it another job, `Tab` picks another window with `1`-`9`,
`Backspace` undoes one press, and `Esc` backs out and then quits. Set
`keys.reset` to the key that opens the overlay: pressing it starts the
choices over, and pressing it with nothing to undo closes the overlay.
Moving the mouse closes it too, since reaching for it says the keyboard
was not what you wanted. `Ctrl+Esc` gets the keyboard back if a run is
killed before it can tidy up.

Colours, label sizing, which keys click, and the limits on reading a
window are set in `~/.config/wl-wysiwyc/config.yaml`, which is optional:
see [docs/configuration.md](docs/configuration.md).

Element hints need AT-SPI accessibility enabled on the system; Chromium
additionally needs a launch flag. See
[docs/how-it-works.md](docs/how-it-works.md) for setup, internals,
debug flags, and current limitations.
