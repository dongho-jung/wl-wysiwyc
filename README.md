# wl-wysiwyc

A keyboard-driven pointer for Wayland. It labels clickable elements in the
focused window and falls back to a screen-aligned keyboard grid when no
accessibility tree is available. Hyprland only.

## Build and run

```sh
cargo build --release
./target/release/wl-wysiwyc
```

## Documentation

- [Usage](docs/usage.md)
- [Configuration](docs/configuration.md)
- [How it works, setup, debugging, and limitations](docs/how-it-works.md)
