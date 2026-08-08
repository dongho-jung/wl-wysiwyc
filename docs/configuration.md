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

  # The keys that click. Either an xkb key name or the character itself,
  # which is translated to the name the compositor wants. A letter here
  # is kept out of the hints and the grid, so it never means two things.
  # Mind that a bare - or = is YAML syntax: quote it, or use the name.
  left_click: minus
  right_click: equal

label:
  size: 11.5    # text size
  pad_x: 4.5    # space either side of the text
  pad_y: 3.0    # space above and below
  gap: 3.0      # clearance kept between labels
  track: 2.5    # space between a label's characters

colors:
  dim: "#00000047"          # laid over the output, so labels stand out
  shadow: "#0000006b"       # under every label
  hint: "#fac94ae0"         # a hint waiting to be typed
  hint_text: "#241700"
  armed: "#3dd999eb"        # the target picked out, or armed
  armed_text: "#002414"
  armed_key: "#053d26eb"    # the armed key, shown pressed in them
  armed_key_text: "#8cffd1"
  ring: "#40eba1f2"         # around the element about to be clicked
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
- A click key does not have to be one of the two defaults. Any key xkb
  can name works, and if it is a letter the hints and the grid make room
  by leaving that letter out.
- Labels are slightly transparent on purpose, so a small icon under one
  is still recognisable. End `hint` with `ff` for solid labels.
- Raising `elements.walk_ms` gets more of a heavy page hinted at the
  cost of a longer pause before the overlay appears. See the limitation
  in [how-it-works.md](how-it-works.md) for why the walk is slow.
- Nothing here changes which keys are used. The three qwerty rows are
  laid over the window to decide labels, and the grid is those same
  rows, so a different layout would change what the labels mean.
