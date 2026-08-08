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
# How long an armed key waits before confirming itself, in ms. The
# second press is a shortcut past this wait rather than the only way
# through. Set it to 0 to always require the second press.
confirm_ms: 300

label:
  size: 11.5    # text size
  pad_x: 4.5    # space either side of the text
  pad_y: 3.0    # space above and below
  gap: 3.0      # clearance kept between labels
  track: 2.5    # space between a label's characters

colors:
  dim: "#00000047"          # laid over the output, so labels stand out
  shadow: "#0000006b"       # under every label
  hint: "#fac94af7"         # a hint waiting to be typed
  hint_text: "#241700"
  armed: "#3dd999fa"        # a hint the armed key would keep
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

- `confirm_ms` is the one to reach for first. Lower it and the overlay
  feels like typing; raise it and every key waits to be told twice.
- Raising `elements.walk_ms` gets more of a heavy page hinted at the
  cost of a longer pause before the overlay appears. See the limitation
  in [how-it-works.md](how-it-works.md) for why the walk is slow.
- Nothing here changes which keys are used. The three qwerty rows are
  laid over the window to decide labels, and the grid is those same
  rows, so a different layout would change what the labels mean.
