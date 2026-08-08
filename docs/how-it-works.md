# How it works

The tool runs in three steps: snapshot, overlay, click. The overlay
opens on the focused window in one of two modes: element hints
(preferred) and the letter grid (fallback).

## Snapshot

`src/hypr.rs` shells out to `hyprctl -j monitors` and `hyprctl -j
clients`. It keeps windows that are mapped, not hidden, visible (this
drops inactive tabs of a Hyprland group), and on the active workspace
of the focused monitor. Windows are sorted top-to-bottom then
left-to-right and capped at nine, since picker keys are 1-9; the
focused window (`focusHistoryID` 0) keeps its place even when it sorts
past the ninth, because that is where the overlay starts. The snapshot
also records each window's pid, the focused monitor's logical geometry,
and the bounding box of the whole output layout.

## Overlay

`src/overlay.rs` connects to the compositor with smithay-client-toolkit
and creates one wlr-layer-shell surface on the overlay layer, anchored
to all edges of the focused output, with exclusive keyboard
interactivity. Rendering is plain software drawing into a shared-memory
Argb8888 buffer (`src/draw.rs`): premultiplied alpha and glyphs
rasterized with fontdue. Every shape is a rounded rectangle measured by
its signed distance, which is what gives the labels smooth corners,
outlines of any width, and shadows, all from the same few lines. The
font comes from `fc-match sans:bold` with fallbacks to common system
paths. Buffers are rendered at an integer scale of ceil(monitor scale)
so text stays sharp on fractional-scale outputs.

Startup queries the focused window's clickable elements over AT-SPI
(1.8 s hard timeout, results cached per window). If elements are found
the overlay opens in hint mode; otherwise it falls back to the grid.
Tab opens the window picker, which draws a number on every window and
switches to that window on 1-9.

### Arming

No key press commits on its own. The first press of a key arms it:
whatever that key would select turns green and the rest steps back.
Pressing the same key again, or Enter, confirms it, and confirming the
last key of a hint clicks that element. Another key moves the preview,
Backspace undoes one press, and a key that leads nowhere is ignored.

An armed key that leaves a single candidate is the press that clicks,
so it earns a ring and a glow around the element while a press that
only narrows the field does not. Confirmed keys stay in their label and
dim in place, which keeps a label the same size from first press to
last and shows how far along it is.

So a hint of DJ is d, d, j, j: two keys, each confirmed separately.
This holds in both modes, and it is what makes the overlay usable
without looking at the keyboard.

### Hint mode

Every clickable element gets an amber label (`src/hint.rs`). The first
key is where the element is: the qwerty block is laid over the window
the way the grid is, and an element takes the key covering it, never a
neighbour's. An element in the bottom-left corner is labelled Z and one
in the top-right P, however the elements happen to be spread, so a
window whose targets crowd into one strip keeps them on the keys under
that strip instead of fanning them across the keyboard.

A key covering one element is the whole label. A key covering several
becomes their prefix, and those elements take a second key handed out
along the group in reading order: the first key has already said where
the group is, so the rest of the label uses the keyboard end to end
rather than squeezing a column of elements into the three keys above
each other. Labels are prefix-free, so a complete label is never the
start of another one.

Each confirmed key narrows the visible hints. Labels sit at the
top-left corner of their element, vimium style, but a label that would
land on one already placed, or right up against it, tries the element's
other corners and sides first. Element outlines are drawn only once a
key has narrowed the field, since outlining everything at once is
noise. Esc drops the armed key, then the confirmed ones, then quits.
Space switches to the grid for spots the tree does not cover.

### Grid mode

The window is divided into three rows following the qwerty layout
(`src/grid.rs`): ten tiles for q-p, nine for a-l, seven for z-m. A
letter arms that tile and the same letter again clicks its center, the
same two presses as a one-key hint. Space switches back to hint mode
when elements exist.

## Element discovery (AT-SPI)

`src/atspi.rs` talks to the accessibility bus directly over zbus:

1. Ask `org.a11y.Bus` on the session bus for the a11y bus address and
   connect to it.
2. List applications from the registry root and match the window by
   comparing each connection's `GetConnectionUnixProcessID` against the
   window's pid from Hyprland.
3. Pick the frame whose accessible name equals the window title (falls
   back to the first frame).
4. Breadth-first walk: prune subtrees whose root lacks the SHOWING
   state, collect nodes whose role is interactive (button, link, entry,
   check box, combo box, menu item, tab, slider, list item, and so on)
   and SENSITIVE, and read their extents in window coordinates.
   Budgets: 4000 nodes, 400 elements, 1.2 s. Running out of budget is
   reported on stderr, because the walk is breadth-first and a heavy
   page then keeps its chrome and loses part of its content.
5. Pruning: trees nest a link inside a list row inside a cell, all with
   near-identical extents, and one hint per level buries the window in
   labels. A row or tree item that wraps another clickable element
   gives way to it, and what is left keeps one element per spot.
6. Coordinate correction: toolkits report window-relative coordinates
   under Wayland (they do not know their global position), so the
   window's global position from Hyprland is added afterwards. Chromium
   additionally reports web-content extents in physical pixels while
   its browser UI uses logical pixels; at each `document web` node the
   ratio of its extents to its parent's is divided back out of every
   descendant.

Measured on this setup: a small page yields 17 elements in about 0.1 s,
the GeekNews front page 57 elements in about 0.1 s.

## System setup required for hints

- GTK and Qt applications publish their trees when the desktop
  advertises an assistive technology:
  `gsettings set org.gnome.desktop.interface toolkit-accessibility true`
  and
  `gsettings set org.gnome.desktop.a11y.applications screen-reader-enabled true`.
  GTK generally registers even without them; Qt reads them at startup.
- Chromium ignores those signals (verified on Chromium 150) and needs
  `--force-renderer-accessibility`, easiest as a line in
  `~/.config/chromium-flags.conf`. Already-running instances need a
  restart to pick it up.
- Electron apps bundle their own Chromium and do not read
  chromium-flags.conf; they need the same flag passed by their own
  launcher wrapper.
- Applications that draw their own UI (kitty and other GPU terminals,
  mpv, games) have no accessibility tree at all; the grid fallback
  covers them.

## Click

The chosen point (tile center or element center) is recorded in global
logical coordinates, the overlay is torn down (so the click cannot land
on the overlay itself), and the click is injected through
`zwlr_virtual_pointer_v1`: absolute motion scaled to the output layout
extent, then a left button press and release.

## Debug flags

- `--list` prints the snapshot: monitor, layout extent, and the
  windows with their geometry, marking the focused one.
- `--elements [N]` prints the clickable elements detected for window N,
  or for the focused window: the hint each would get, its role, and its
  window-relative rectangle.
- `--smoke MS [N]` renders the hint overlay for MS milliseconds without
  grabbing the keyboard, for window N or the focused window.
- `--smoke-grid MS [N]` is the same for the letter grid,
  `--smoke-pick MS` for the window picker.
- `--render FILE [N [KEYS]]` writes what the overlay would draw to a
  binary PPM, over flat grey, without showing anything on screen. KEYS
  is a run of presses, all but the last confirmed and the last one
  armed, so `--render out.ppm 2 qw` is what the overlay looks like with
  Q confirmed and W armed. The only way to see an armed overlay without
  holding the keyboard, and it leaves the desktop alone.
- `--move-test X Y` moves the cursor to global (X, Y) through the
  virtual pointer without clicking. Verifies coordinate mapping.

## Current limitations

- Hyprland only; window discovery is tied to its IPC. Other wlr-based
  compositors would need their own snapshot source.
- Only the focused monitor's active workspace is shown. Special
  (scratchpad) workspaces and pinned windows are not handled.
- Hint clicks target the element center; elements overlapped by
  something else (sticky headers, popovers) can be misclicked.
- Heavy pages outrun the 1.2 s walk budget, which leaves their content
  partly unhinted (the grid still covers it). The walk costs three
  D-Bus round trips per node; `org.a11y.atspi.Cache.GetItems` would
  fetch roles, states, and the tree shape in one call instead.
- Left click only, and the cursor stays where the click happened.
- Monitors left of or above the origin (negative layout coordinates)
  are not supported.
- Rotated monitors are untested.
