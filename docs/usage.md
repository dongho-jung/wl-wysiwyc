# Usage

wl-wysiwyc opens over the focused window. It uses labels when the application
exposes clickable elements through AT-SPI and a keyboard-shaped grid otherwise.
The initial target is the anchor nearest the pointer.

## Choose a target

Type a label to move to that element. In grid mode, press a letter to move to
the center of its tile. Labels follow the configured keyboard layout, so their
first key reflects the target's position in the window. Labels sit near the
center when the target is large enough to remain visible. Small and compact
icon targets keep their labels just outside instead, and nearby labels avoid
covering them too. Nearby labels move only as far as needed to avoid
overlapping.

`Space` switches between labels and the grid unless it has another configured
job. `Tab` opens the window picker, then `1` through `9` selects a window.

## Move with arrows

| Input | Action |
| --- | --- |
| `arrow` | Accelerate freely, coast on release, then settle on an anchor |
| `Shift+arrow` | Scroll in that direction, repeating while held |
| `Ctrl+arrow` | Jump to the next anchor in that direction |
| `Alt+arrow` | Move at a constant speed and stop at the released position |

Two arrows move diagonally. Pushing against a window edge scrolls on that axis.
During pointer movement, labels temporarily become red anchor dots and the
nearest anchor is blue.

## Click and drag

`-` and `Enter` hold the left mouse button. `=` holds the right mouse button.
Releasing the key releases the button and closes the overlay. Move with arrows
or choose another label while the key is held to drag. Shift, Ctrl, and Alt are
passed through with the click. After using Shift+arrow to scroll, release and
press Shift again before a Shift-click.

The click keys are configurable, and several keys can control the same button.
See [Configuration](configuration.md).

## Scroll and leave

- `;` scrolls up and `'` scrolls down without closing the overlay.
- `Shift+arrow` scrolls without closing the overlay. Up and down scroll
  vertically, while left and right scroll horizontally.
- Scrolling replaces labels with anchor dots and leaves that view in place.
  Type any alphabetic key to restore labels and continue typing a hint. Input
  received while the hint refresh is settling is replayed when it is ready.
- `Backspace` undoes one key press.
- `Esc` backs out, then closes the overlay.
- Moving the physical mouse closes the overlay.
- `Ctrl+Esc` restores the keyboard if a run ends before it can leave the
  compositor submap.

Set `keys.reset` to the key that launches wl-wysiwyc if that key should reset
the current choices and close the overlay when there is nothing left to undo.

## Element hint setup

Element hints require AT-SPI accessibility. Chromium and Electron applications
also need an accessibility launch flag. The grid remains available when an
application exposes no accessibility tree. See
the [hint setup guide](how-it-works.md#system-setup-required-for-hints).
