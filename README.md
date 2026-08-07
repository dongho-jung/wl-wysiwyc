# wl-wysiwyc

What You See Is What You Click for Wayland. Run it, pick a window by
number, pick a spot by letter, and it clicks there. No mouse needed.

Currently supports Hyprland.

## Install and run

```
cargo build --release
./target/release/wl-wysiwyc
```

Keys: `1`-`9` select a window, `a`-`z` click the matching qwerty tile,
`Esc` goes back or quits.

See [docs/how-it-works.md](docs/how-it-works.md) for internals, debug
flags, and current limitations.
