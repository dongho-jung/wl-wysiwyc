# How it works

The tool runs in three steps: snapshot, overlay, click.

## Snapshot

`src/hypr.rs` shells out to `hyprctl -j monitors` and `hyprctl -j
clients`. It keeps windows that are mapped, not hidden, visible (this
drops inactive tabs of a Hyprland group), and on the active workspace
of the focused monitor. Windows are sorted top-to-bottom then
left-to-right and capped at nine, since selection keys are 1-9. The
snapshot also records the focused monitor's logical geometry and the
bounding box of the whole output layout.

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

Stage one draws a numbered label on every window. Pressing a digit
switches to stage two, which divides the chosen window into three rows
following the qwerty layout (`src/grid.rs`): ten tiles for q-p, nine
for a-l, seven for z-m. Esc returns to stage one, and Esc again quits.

## Click

Pressing a letter records the center of that tile in global logical
coordinates, tears down the overlay (so the click cannot land on the
overlay itself), and injects the click through
`zwlr_virtual_pointer_v1`: absolute motion scaled to the output layout
extent, then a left button press and release.

## Debug flags

- `--list` prints the snapshot: monitor, layout extent, and the
  windows with their geometry.
- `--smoke MS [N]` renders the overlay for MS milliseconds without
  grabbing the keyboard. With N it shows the letter grid for window N.
  Useful for screenshots and render checks.
- `--move-test X Y` moves the cursor to global (X, Y) through the
  virtual pointer without clicking. Verifies coordinate mapping.

## Current limitations

- Hyprland only; window discovery is tied to its IPC. Other wlr-based
  compositors would need their own snapshot source.
- Only the focused monitor's active workspace is shown. Special
  (scratchpad) workspaces and pinned windows are not handled.
- One click precision level: 26 tiles per window, no refinement stage.
- Left click only, and the cursor stays where the click happened.
- Monitors left of or above the origin (negative layout coordinates)
  are not supported.
- Rotated monitors are untested.
