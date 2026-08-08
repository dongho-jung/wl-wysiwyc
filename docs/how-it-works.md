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
Argb8888 buffer (`src/draw.rs`): premultiplied alpha, rectangles, and
glyphs rasterized with fontdue. The font comes from `fc-match
sans:bold` with fallbacks to common system paths. Buffers are rendered
at an integer scale of ceil(monitor scale) so text stays sharp on
fractional-scale outputs.

Startup queries the focused window's clickable elements over AT-SPI
(1.8 s hard timeout, results cached per window). If elements are found
the overlay opens in hint mode; otherwise it falls back to the grid.
Tab opens the window picker, which draws a number on every window and
switches to that window on 1-9.

### Arming

No single key press clicks anything. A key press picks a target and
arms it: the target turns green, and pressing the same key again (or
Enter) clicks it. Any other key picks a different target instead, so a
mistyped hint needs no undo. This holds in both modes, and it is what
makes the overlay usable without looking at the keyboard.

### Hint mode

Every clickable element gets an amber label (`src/hint.rs`). Position
picks the key: the three qwerty rows are laid over the elements the way
the grid is laid over a window, so an element in the top-left corner is
labelled near Q and one in the bottom-right near M. One key per element
while 26 suffice; past that the key becomes a prefix and the elements
that share it are labelled the same way again, which keeps labels short
where the window is sparse and grows them only where it is crowded.
Labels are prefix-free, so a complete label is never the start of
another one.

Typing narrows the visible hints, and the press that leaves a single
candidate arms it. Backspace steps back, Esc clears the typed prefix
and then quits, Space switches to the grid for spots the tree does not
cover.

### Grid mode

The window is divided into three rows following the qwerty layout
(`src/grid.rs`): ten tiles for q-p, nine for a-l, seven for z-m. A
letter arms that tile, the same letter again clicks its center. Space
switches back to hint mode when elements exist.

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
   Budgets: 4000 nodes, 400 elements, 1.2 s.
5. Coordinate correction: toolkits report window-relative coordinates
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
  or for the focused window, with roles and window-relative rectangles.
- `--smoke MS [N]` renders the hint overlay for MS milliseconds without
  grabbing the keyboard, for window N or the focused window.
- `--smoke-grid MS [N]` is the same for the letter grid,
  `--smoke-pick MS` for the window picker.
- `--move-test X Y` moves the cursor to global (X, Y) through the
  virtual pointer without clicking. Verifies coordinate mapping.

## Current limitations

- Hyprland only; window discovery is tied to its IPC. Other wlr-based
  compositors would need their own snapshot source.
- Only the focused monitor's active workspace is shown. Special
  (scratchpad) workspaces and pinned windows are not handled.
- Hint clicks target the element center; elements overlapped by
  something else (sticky headers, popovers) can be misclicked.
- Left click only, and the cursor stays where the click happened.
- Monitors left of or above the origin (negative layout coordinates)
  are not supported.
- Rotated monitors are untested.
