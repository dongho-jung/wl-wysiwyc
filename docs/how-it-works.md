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
to all edges of the focused output, with an empty input region. Taking
no pointer input matters: a surface that accepts it pulls the pointer
off the window underneath, which sees the pointer leave, and a panel
held open by hover folds up as the overlay opens.

Rendering is plain software drawing into a shared-memory Argb8888 buffer
(`src/draw.rs`): premultiplied alpha and glyphs rasterized with fontdue.
Every shape is a rounded rectangle measured by its signed distance,
which is what gives the labels smooth corners, outlines of any width and
shadows, all from the same few lines. The font comes from `fc-match
sans:bold` with fallbacks to common system paths. Buffers are rendered
at an integer scale of ceil(monitor scale) so text stays sharp on
fractional-scale outputs.

A frame is a whole output, fifty megabytes of it, and the pointer's
travel is drawn one frame at a time, so three things keep a frame cheap:
the distance measurement is only taken near a shape's edge, since the
middle of a shape is either covered or not; glyph rasters are kept
between frames rather than drawn again each time; and the blending is
integer arithmetic.

Startup queries the focused window's clickable elements over AT-SPI
(1.8 s hard timeout, results cached per window). If elements are found
the overlay opens in hint mode; otherwise it falls back to the grid.
Tab opens the window picker, which draws a number on every window and
switches to that window on 1-9.

### Keys

The overlay does not hold the keyboard either, for the same reason. A
layer surface that asks for it takes activation away from the window
underneath, and a window that loses activation drops its hover state:
Chromium folds up a menu that was only open because the pointer was on
it, so the hints again end up describing a window that is no longer
there.

The keys come from the compositor instead. `src/shortcuts.rs` registers
one Hyprland global shortcut per key the overlay listens for, and
`src/hypr.rs` defines a submap binding each key to its shortcut, then
enters it. Triggering a shortcut delivers an event without touching
focus, so the window under the overlay stays exactly as it was, pointer
and activation and hover included.

A submap cannot be cleared, and defining one twice appends a second copy
of every bind, which would make one press count as two. So the submap's
name carries a digest of the keys in it: the same keys find the same
submap already defined, and changing which keys the overlay wants means
a new submap rather than a redefinition. Hyprland takes its config
either as Lua or in its own language, and only one of the two answers a
given call, so both forms are tried.

Getting out of the submap has three guards, because every key in it
dispatches to this client and being stuck there with the client gone
would leave the keyboard useless. The overlay leaves on the way out. A
watchdog process, detached from this one's process group, leaves on its
behalf if it is killed rather than asked to quit, since a killed process
runs no destructors. And the submap binds `Ctrl+Esc` straight to leaving
it, which works whatever state this process is in, as does
`wl-wysiwyc --reset` from a terminal.

The window's elements are read before the submap goes up rather than
after, so the keyboard is only taken once the overlay can answer for it.
For the same reason the overlay gives up if the compositor has not
configured its surface within three seconds.

A compositor without the protocol, or a submap that will not take,
falls back to holding the keyboard the ordinary way, hover cost and
all.

### What a key press does

One press is one key. A key that means nothing where you are is
ignored, so a hint typed at speed is one press per character.

Completing a hint or picking a grid tile does not click. It puts the
pointer on that target and stops there, marked green with a ring, and
then `left_click` (Enter by default) clicks it, `right_click`
(backslash) right-clicks it, and typing another hint moves the target
somewhere else. Moving the pointer is the point of the pause as much as
the choice of button is: it is how the window under it shows a hover,
and how a menu that only opens on hover can be opened at all.

Two settings change this. `keys.instant` clicks the moment a hint is
complete, skipping the pause and the choice. `keys.confirm` asks for
every key twice: the first press arms it, and until it is pressed again
nothing has been selected. An armed key is drawn pressed inside every
label it would keep, a dark cap over that one character, and those
labels turn green.

The overlay opens with the target already on whatever the pointer is
nearest, so a click can need no typing at all, and the arrow keys move
it from there. A step goes to the nearest target roughly in line that
way, and only widens its search when nothing is in line, which is what
walks a row of buttons or a column of list items without surprises. The
pointer travels to a new target over about a tenth of a second rather
than jumping, since a pointer that jumps leaves you hunting for it.

Esc unwinds one step at a time: the armed key, then the target, then
what was typed, then the overlay. Backspace undoes one key press. Space
swaps element hints for the letter grid. Tab opens the window picker.

`keys.reset` is worth setting to whatever key opens the overlay. While
the overlay is up its keys live in a compositor submap, and a submap
answers only for the keys in it, so the keybind that started the overlay
cannot fire again: that key does nothing unless the overlay claims it.
Claimed, it puts the choices back to how the overlay opened, which is
what a key pressed by mistake should do, and pressing it with nothing to
undo closes the overlay. So the key that opens it also gets you out:
once to abandon a half-typed hint, again to leave. Launching the tool
again from anywhere else, a terminal or a second keybind, cancels the
overlay instead.

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

Each key narrows the visible hints. Labels sit at the top-left corner of
their element, vimium style, unless the element is small enough for the
label to swallow it, in which case the label goes beside it: a row of
icons is unusable when every icon is under a label. Beside is not enough
on its own, since the icon next door is worth seeing too, so of the
places that clear the labels already put down, the one covering the
least of everything else wins. Labels are also a little transparent, so
whatever a label does end up on is still recognisable. Only the element
about to be clicked is outlined, since ringing every candidate turns a
dense corner of a window into a mess of boxes.

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
4. Breadth-first walk: collect nodes that are SHOWING and SENSITIVE and
   whose role is interactive (button, link, entry, check box, combo box,
   menu item, tab, slider, list item, and so on), reading their extents
   in window coordinates. Budgets: 4000 nodes, 400 elements, 1.2 s.
   Running out of budget is reported on stderr, because the walk is
   breadth-first and a heavy page then keeps its chrome and loses part
   of its content.
   A node costs four calls (role, state, extents, children) and a window
   runs to several hundred nodes, so the walk reads 32 nodes at a time
   with a thread each. One zbus connection carries them all: replies are
   matched to calls by serial, so the calls overlap and a batch costs
   about what the application takes to answer it rather than a round
   trip apiece. That is worth roughly six times the speed, and it is the
   difference between the overlay appearing at once and appearing after
   a visible pause.
5. Viewport: a node whose rectangle sits wholly outside the window, or
   which is not SHOWING, goes to the back of the walk along with
   everything inside it. A long page has far more of those than it has
   visible ones, and walking them first is what spends the budget
   before the walk reaches the part of the page on screen. It is an
   order and not a prune because Chromium gives a scroll container the
   extents of its whole contents: on a scrolled page every ancestor of
   what you can see reports a rectangle nowhere near the window, and
   half of them are not SHOWING either, while the rows inside them are
   placed correctly.
6. Pruning: trees nest a link inside a list row inside a cell, all with
   near-identical extents, and one hint per level buries the window in
   labels. A row or tree item that wraps another clickable element
   gives way to it, and what is left keeps one element per spot.
7. Coordinate correction: toolkits report window-relative coordinates
   under Wayland (they do not know their global position), so the
   window's global position from Hyprland is added afterwards. Chromium
   additionally reports web-content extents in physical pixels while
   its browser UI uses logical pixels; at each `document web` node the
   ratio of its extents to its parent's is divided back out of every
   descendant.

Measured on this setup: a small page yields 17 elements in about 0.05 s
and a scrolled Portainer service page 130 elements in about 0.11 s,
which puts the whole overlay on screen in about 0.16 s. The walk starts
before the font and the Wayland surface do, so most of the rest of the
startup happens while it runs.

Chromium builds its accessibility tree as it is asked for, so the first
walk of a page it has never been asked about finds less than the second:
the counts above are what it settles on.

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
- Electron apps (Slack, Discord, VS Code) bundle their own Chromium and
  do not read chromium-flags.conf, so the flag has to go on their own
  command line. For a packaged app that means copying its desktop entry
  to `~/.local/share/applications/` and adding the flag to `Exec=`:

  ```
  Exec=/usr/bin/slack --force-renderer-accessibility -s %U
  ```

  Without it the application is absent from the accessibility bus
  entirely, which the overlay reports on stderr before falling back to
  the grid.
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
  binary PAM, alpha and all, without showing anything on screen. KEYS
  is a run of presses, all but the last confirmed and the last one
  armed, so `--render out.pam 2 qw` is the overlay with Q confirmed and
  W armed; a trailing `.` confirms them all and arms nothing. Laid over
  a screenshot it is exactly what would have been on screen:

  ```
  grim -o HDMI-A-1 desk.png
  wl-wysiwyc --render over.pam 2 q
  ffmpeg -i desk.png -i over.pam -filter_complex "[0][1]overlay" out.png
  ```

  This is the only way to look at an armed overlay, since the smoke
  runs do not take the keyboard, and it leaves the desktop alone.
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
