# Configuration

wl-wysiwyc reads `~/.config/wl-wysiwyc/config.yaml` once at startup, or
`$XDG_CONFIG_HOME/wl-wysiwyc/config.yaml` when that is set. The file is
optional and so is every key in it: anything left out keeps its default.
A file that will not parse is reported on stderr and then ignored, so a
typo in a colour cannot stop the overlay from opening. An unknown key is
an error rather than a shrug, which is how a misspelling gets noticed.

Colours are hex, with or without the hash: `#rgb`, `#rrggbb`, or
`#rrggbbaa` to set opacity. Lengths are in unscaled pixels and are
multiplied by the output's integer scale when drawn.

## Everything, at its default

```yaml
keys:
  # Ask for every key twice: the first press shows what it would select,
  # the second takes it. Nothing is selected by one keystroke.
  confirm: false

  # Click as soon as a hint is complete. With this off, a complete hint
  # puts the pointer on its target and waits, which leaves room to look
  # before clicking and to choose which button.
  instant: false

  # The keys that click, either one or a list of them. Either an xkb key
  # name or the character itself, which is translated to the name the
  # compositor wants (enter is spelled return). A letter here is kept
  # out of hints and the grid, so it never means two things. Mind that
  # a bare - or = is YAML syntax: quote it, or use the name.
  left_click: [minus, return]
  right_click: equal

  # Swaps element hints for the letter grid. Give this key to a click
  # and it stops switching, since a key does one job; name another key
  # here to keep the grid within reach. Empty means none.
  switch: space

  # The keys that scroll the window under the pointer, without leaving
  # the overlay. Shift with one goes to the end, ctrl turns the pair
  # sideways, so ctrl scroll_up scrolls left. Empty means none.
  scroll_up: semicolon
  scroll_down: apostrophe

pointer:
  # How far the mouse has to be moved by hand before the overlay gets
  # out of the way. Reaching for the mouse says the keyboard was not
  # what you wanted after all; a knock on the desk does not, so it takes
  # a deliberate distance. 0 leaves the overlay up whatever the mouse
  # does, and saves asking the compositor where the pointer is eight
  # times a second while the overlay is open.
  cancel_px: 24

  # A press of an arrow key steps to the next target that way. Holding
  # it steps again after repeat_ms, and each wait after that is shorter
  # than the last, down to repeat_min_ms.
  repeat_ms: 280
  repeat_min_ms: 70

  # How far a tap of an arrow key looks for the next target that way.
  # Past this it does nothing rather than throw the pointer across the
  # window; hold the key to fly there instead.
  reach_px: 260

  # About how long the pointer takes to reach a target it was sent to
  # by name, by typing a hint. It is pulled there rather than put there,
  # so this sets how hard the pull is rather than timing the trip.
  travel_ms: 280

scroll:
  # Wheel notches per press, and per press with shift. There is no
  # scroll-to-the-end on a wheel, only more of it, so shift sends a run
  # of notches: raise far for a document it does not get to the end of.
  step: 3
  far: 200

  # Reading a window takes long enough to be worth doing once. Scrolling
  # moves everything the hints named, so they fade until this long after
  # the last scroll, and then the window is read again.
  settle_ms: 120

click:
  # A click key clicks when it is let go, and how long it was held says
  # how many times: a tap clicks once, holding past double_ms clicks
  # twice, past triple_ms three times. Zero on either turns that step
  # off, so double_ms: 0 leaves every click single.
  double_ms: 450
  triple_ms: 1100

  # Show the charge while the key is down: a ring closing in on the
  # target as the step fills, a wave as it lands, and a badge saying
  # what letting go now would click, rather than a count timed in the
  # head.
  charge: true

  # The keyboard the labels are laid out on, so that where something is
  # on screen decides which key names it: qwerty, dvorak, or none. With
  # none there is no arrangement to follow and labels are handed out a
  # to z in reading order instead.
  layout: qwerty

  # Letters to keep out of hints and the grid, run together. For the
  # keys you would rather not reach for: excluded: tyughvbn leaves the
  # eighteen that stay under a hand.
  excluded: ""

  # An extra key that puts the overlay back to how it opened, and closes
  # it when there is nothing left to undo. Worth setting to whatever key
  # opens the overlay: while the overlay is up that key belongs to the
  # overlay rather than to the keybind that started it, so pressing it
  # again undoes a wrong turn instead of doing nothing, and pressing
  # it once more gets you out. Empty means none.
  reset: ""

label:
  size: 11.5    # text size
  pad_x: 4.5    # space either side of the text
  pad_y: 3.0    # space above and below
  gap: 3.0      # clearance kept between labels
  track: 2.5    # space between a label's characters
  wake_ms: 700  # how long after the last arrow step the labels return

colors:
  dim: "#00000000"          # laid over the output; add alpha to darken
  shadow: "#0000006b"       # under every label
  hint: "#fac94ae0"         # a hint waiting to be typed
  hint_text: "#241700"
  armed: "#3dd999eb"        # the target picked out, or armed
  armed_text: "#002414"
  armed_key: "#053d26eb"    # the armed key, shown pressed in them
  armed_key_text: "#8cffd1"
  ring: "#40eba1f2"         # around the element about to be clicked
  charge: "#fac94af2"       # the first level of a held click key
  dot: "#e83e33f0"          # a target while the arrows are stepping
  tile: "#14141a33"         # grid tiles
  tile_border: "#ffffff4d"
  text: "#fffffff5"         # grid letters and window numbers

elements:
  max: 400        # stop after this many elements in one window
  walk_ms: 1200   # give up walking the tree after this long
  query_ms: 1800  # hard limit, for a toolkit that never answers
```

## Notes

- The two `keys` switches are worth trying first. The default is the
  quickest: type the hint, then `-` to click. `instant: true` drops that
  press and clicks the moment the hint is complete, faster and less
  forgiving. `confirm: true` goes the other way and wants every key
  twice.
- A click key does not have to be one of the defaults. Any key xkb can
  name works, and if it is a letter the hints and the grid make room by
  leaving that letter out. The same goes for `reset` and `switch`.
- Several keys can click. `left_click: [minus, space, enter]` clicks on
  any of the three. They are one shortcut with three keys on it, not
  three shortcuts.
- Shift, ctrl and alt with a click key are passed through: the overlay
  never holds the keyboard, so the window already knows which modifiers
  are down when the click lands, and holding one only stops the
  compositor giving the combination back to the window as a keystroke.
- A hold is called off by pressing anything else, so a key held by
  mistake costs nothing, and nothing is clicked until the key comes up
  however long it is held.
- Excluding letters costs capacity, not correctness: fewer keys means
  more windows need two-key hints, and a window with more targets than
  the keys left can name will use three. Eighteen letters still name
  324 targets in two keys, which is more than the element limit below.
- In the grid, an excluded letter's tile is not left empty; its row
  spreads out to fill the width, so every part of the window stays
  reachable.
- Labels are slightly transparent on purpose, so a small icon under one
  is still recognisable. End `hint` with `ff` for solid labels.
- `dim` is transparent by default. The labels carry themselves against
  most windows, and darkening the screen to read them is a tax on every
  glance; `#00000047` is the old look if the contrast is wanted.
- Raising `elements.walk_ms` gets more of a heavy page hinted at the
  cost of a longer pause before the overlay appears. See the limitation
  in [how-it-works.md](how-it-works.md) for why the walk is slow.
- Nothing here changes which keys are used. The three qwerty rows are
  laid over the window to decide labels, and the grid is those same
  rows, so a different layout would change what the labels mean.
