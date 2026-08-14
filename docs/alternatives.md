# Wayland alternatives

wl-wysiwyc is one of several tools that turn the keyboard into a pointing
device. The important differences are not just key bindings. The tools vary in
how they find a target, which Wayland compositors they can use, whether they
need privileged input access, and whether clicking and dragging remain part of
the same interaction.

This comparison covers tools that run in a native Wayland session. It excludes
X11-only projects such as keynav, operating-system-specific tools with no Linux
Wayland version, browser-only extensions, and general automation tools without
an interactive keyboard pointer interface.

The comparison is based on upstream documentation and source as reviewed on
2026-08-14. It is a design comparison, not a latency benchmark. Wayland support
is often compositor-specific, so check the linked project before installing a
tool on a compositor not named here.

## Comparison

| Tool | How it chooses a target | When no accessibility tree exists | Actions in the interaction | Wayland scope and input path |
| --- | --- | --- | --- | --- |
| wl-wysiwyc | Clickable AT-SPI elements in the focused window, with spatial labels | One-key, keyboard-shaped grid plus free and anchored arrow movement | Left or right click, drag, two-axis scroll, free movement, anchor jumps, and window picking | Hyprland only; layer shell, Hyprland global shortcuts, and the wlr virtual-pointer protocol; no uinput or screenshot access |
| [Hints](https://github.com/AlfredoSequeida/hints) | AT-SPI elements in the focused window | OpenCV contour detection over a screenshot | Single or repeated click, right click, hover, drag, movement, and a separate scroll mode | Per-compositor adapters for several Wayland desktops; a daemon writes to uinput and needs input-device permissions |
| [wl-kbptr](https://github.com/moverest/wl-kbptr) | User-supplied or OpenCV-detected rectangles, labeled tiles, bisection, or repeated splits | Its coordinate modes always remain available | Selection and clicking; continuous movement, button holds, and scrolling are normally composed with compositor commands | Tested on several compositors that implement the required wlr protocols; optional screencopy supports detection |
| [warpd](https://github.com/rvaiya/warpd) | Screen-wide, evenly distributed hints, recursive grid, or saved target history | All targeting is coordinate-based | Continuous movement, click, drag, scroll, grid, hints, and history in a modal interface | The Wayland port targets Sway and wlroots compositors; upstream documents limited Wayland testing and focus-related caveats |
| [mousefree](https://github.com/swaits/mousefree) | A two-key screen grid followed by variable-size nudges | All targeting is coordinate-based | Click, double-click, triple-click, right click, drag, nudge, and scroll | Compositors with layer shell and the wlr virtual-pointer protocol, including Sway, Hyprland, and river; no uinput required |
| [waywarp](https://github.com/Xuepoo/waywarp) | Screen-wide hints with optional coarse-to-fine refinement | All targeting is coordinate-based | Native continuous movement, click, and scroll, plus callbacks and a programmatic coordinate interface | Hyprland, Sway, river, Wayfire, niri, and other compositors with the required wlr protocols |
| [hyprwarp](https://github.com/bluedeep/hyprwarp) | Multi-monitor coordinate hints | All targeting is coordinate-based | Positioning is built in; callbacks, a Hyprland submap, and tools such as dotool provide clicks, drags, and scrolling | Hyprland only; pointer positioning defaults to Hyprland IPC, while full input control needs an external tool |
| [mouseless](https://github.com/jbensmann/mouseless) | No overlay or target jump; keys continuously steer the pointer | Not applicable | Continuous movement, variable speed, click, button hold, scroll, remapping, and arbitrary input layers | Compositor-independent Linux input through evdev and uinput; normally needs root or input and uinput group access |

### Hints

Hints is the closest match to wl-wysiwyc's semantic targeting. Both read the
focused application's AT-SPI tree and put short labels on exposed controls.
Hints supports more Wayland environments through compositor-specific window
adapters. It also has per-application AT-SPI rules, overlapping-label layers,
repeated clicks, and an OpenCV fallback that can find visible contours when an
application exposes no useful accessibility data.

That breadth has a different setup and failure model. Hints runs a background
daemon and creates uinput devices, so the user needs permission to access input
devices. Its OpenCV backend needs screenshot access, adds large dependencies,
and can label visual edges that are not interactive. Its own documentation
also warns that dragging can vary between Wayland compositors.

### wl-kbptr

wl-kbptr offers several coordinate-selection methods. Tile, bisect, and split
modes do not need application cooperation. Its
floating mode can accept rectangles from another program or detect them from a
screenshot with OpenCV. Modes can be chained, and Backspace can undo a choice
across mode boundaries.

It does not know the semantic difference between a button and a decorative
rectangle. Screenshot detection also requires the optional screencopy protocol
and OpenCV build. Its Hyprland documentation composes wl-kbptr with wlrctl and a
compositor submap for continuous movement, button input, and scrolling, rather
than treating all of those as one built-in session.

### warpd

warpd has a mature modal vocabulary: normal movement, evenly distributed
hints, recursive grid refinement, previous-target history, monitor selection,
dragging, scrolling, and a query interface for scripts. It is a good fit for a
user who wants the same coordinate-driven grammar everywhere and does not need
the tool to understand application controls.

Its upstream documentation describes the Wayland port as Sway/wlroots-only,
minimally tested, and unable to select UI elements that require focus. The
hints are geometric samples, not detected controls, so a final adjustment is
often part of the workflow.

### mousefree

mousefree is a compact, self-contained coordinate workflow. Two keys select a
cell, then the same session can nudge by 1, 8, 16, or 32 pixels, click, drag, or
scroll. It uses normal Wayland protocols instead of uinput.

The trade-off is target awareness. Every destination is a grid cell, and the
current release assumes a US QWERTY layout. It cannot shorten a common button
selection because the application exposes that button through AT-SPI.

### waywarp and hyprwarp

Both tools emphasize coordinate hints, multi-monitor use, and composition with
other commands. waywarp supports compositors that expose the required wlr
protocols, optional coarse-to-fine selection, a continuous pointer mode, and
JSON or direct coordinate commands intended for scripts and agents. hyprwarp
is narrower and uses callbacks as its main extension point: it finds a
coordinate, then lets Hyprland or a tool such as dotool perform the action.

They are attractive when predictable coordinates and automation are more
important than knowing which rectangles are real controls. Neither makes the
accessibility tree the normal targeting source.

### mouseless

mouseless is closer to a configurable keyboard mouse than to a hint overlay.
It can stay running, expose arbitrary input layers, change speed while moving,
and pass unmapped keys through. Since it works below the compositor through
evdev and uinput, it is not tied to a particular Wayland protocol set.

That also means it needs broad input-device access, and it provides no visual
jump to a target. It is strongest for continuous steering and remapping, not
for selecting one of many visible controls with a short label.

## Where wl-wysiwyc is distinct

Both wl-wysiwyc and Hints provide semantic desktop hints on Wayland. AT-SPI
alone is therefore not wl-wysiwyc's distinction. The combination below is.

1. **Semantic fast path with a deterministic fallback.** A GTK, Qt, Chromium,
   or Electron control that exposes a suitable AT-SPI element gets a label at
   its reported hit area. A game, video player, GPU terminal, or inaccessible
   application still gets complete focused-window coverage from the
   keyboard-shaped grid. No screenshot or image threshold decides which
   fallback targets exist.

2. **The target window keeps its interaction state.** The overlay has an empty
   pointer input region. On Hyprland, keys arrive through global shortcuts in a
   compositor submap instead of keyboard focus. The focused application stays
   active, the pointer remains over it, and a hover-open menu does not have to
   collapse merely because the hints appeared.

3. **Selection and manipulation share one session.** Completing a hint moves
   the pointer and leaves the target highlighted. The user can then choose a
   mouse button, hold it and move to drag, scroll without closing the overlay,
   jump between anchors, or steer to an arbitrary point. Scrolling carries the
   visible anchors with the content and refreshes the AT-SPI snapshot after it
   settles.

4. **Labels encode position.** The first character comes from the part of the
   physical keyboard laid over the target's screen position. Dense groups gain
   more characters without losing that first spatial cue. Labels also avoid
   covering small controls when nearby space is available.

5. **The input path needs no privileged daemon.** Pointer events use the
   compositor's virtual-pointer protocol. Element discovery uses the
   accessibility bus. wl-wysiwyc does not read physical keyboards, create
   uinput devices, or capture the screen.

6. **Failure recovery is part of the interaction.** Cancelling releases held
   virtual buttons. A detached watchdog and `Ctrl+Esc` can leave the Hyprland
   submap if the foreground process dies. Deliberate physical mouse movement
   dismisses the overlay.

These choices have a real cost. wl-wysiwyc is Hyprland-only, while Hints,
wl-kbptr, mousefree, and waywarp cover more compositors. It operates on the
focused monitor and active workspace rather than treating every output as one
large hint field. AT-SPI also depends on toolkit cooperation, can take longer
on heavy pages, and sometimes needs an accessibility flag. The grid guarantees
coverage when semantics fail, but a one-key cell is less precise than recursive
bisect or coarse-to-fine coordinate tools until the user steers from it.

## A practical trade-off frontier

There is no single best tool because the useful axes conflict. This is a rough
frontier, not a ranking:

| Priority | Strong candidates | Cost paid for that choice |
| --- | --- | --- |
| Semantic targets, preserved hover state, integrated manipulation, and no privileged input access | wl-wysiwyc | Hyprland-only integration |
| Semantic targets across several Wayland desktops | Hints | Per-compositor adapters and a uinput daemon; the OpenCV fallback needs screenshot access |
| Several precise coordinate-selection algorithms across compatible compositors | wl-kbptr | No application semantics; some actions are composed from external commands |
| A small, integrated coordinate overlay | mousefree | Fixed-grid targeting and US QWERTY only |
| Multi-monitor coordinate selection and automation interfaces | waywarp | No semantic target discovery |
| A long-established modal grammar and target history | warpd | Documented Wayland limitations and coordinate-only hints |
| Persistent continuous movement and keyboard remapping | mouseless | No visual target jump and broad input-device permissions |
| A simple Hyprland coordinate picker with shell callbacks | hyprwarp | External commands are needed for full mouse manipulation |

For a Hyprland user whose common task is "click that visible control without
disturbing the window," wl-wysiwyc occupies a useful corner of this frontier.
For compositor portability, screenshot-based discovery, recursive pixel
refinement, or a permanent keyboard mouse layer, one of the alternatives is a
better fit.

## Sources

- wl-wysiwyc: [usage](usage.md), [implementation and limitations](how-it-works.md)
- Hints: [project README](https://github.com/AlfredoSequeida/hints), [backend configuration](https://github.com/AlfredoSequeida/hints/wiki/Configuration-guide), [Wayland setup](https://github.com/AlfredoSequeida/hints/wiki/Window-Manager-and-Desktop-Environment-Setup-Guide)
- wl-kbptr: [project README](https://github.com/moverest/wl-kbptr)
- warpd: [project README](https://github.com/rvaiya/warpd), [manual](https://github.com/rvaiya/warpd/blob/master/warpd.1.md)
- mousefree: [project README](https://github.com/swaits/mousefree)
- waywarp: [project README](https://github.com/Xuepoo/waywarp), [configuration guide](https://github.com/Xuepoo/waywarp/blob/main/docs/configuration.md)
- hyprwarp: [project README](https://github.com/bluedeep/hyprwarp)
- mouseless: [project README](https://github.com/jbensmann/mouseless)
