//! Keys from the compositor instead of from keyboard focus.
//!
//! A layer surface that asks for the keyboard takes activation away from the
//! window under it, and a window that loses activation drops its hover state:
//! Chromium folds up menus that were only open because the pointer was on
//! them, so the hints end up describing a window that is no longer there.
//!
//! Hyprland's global shortcuts protocol has no such cost. The client
//! registers named actions, the compositor binds keys to them, and triggering
//! one delivers an event without touching focus. The keys only mean anything
//! while the overlay is up, so they live in a submap the overlay enters on
//! start and leaves on exit.

use crate::hypr;
use wayland_client::{globals::GlobalList, Dispatch, QueueHandle};

pub mod protocol {
    #![allow(clippy::too_many_arguments)]
    // The generated code reaches for these by name, from the module they are
    // generated into.
    use wayland_client;

    pub mod __interfaces {
        use wayland_backend;
        wayland_scanner::generate_interfaces!("protocols/hyprland-global-shortcuts-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("protocols/hyprland-global-shortcuts-v1.xml");
}

use protocol::hyprland_global_shortcut_v1::HyprlandGlobalShortcutV1;
use protocol::hyprland_global_shortcuts_manager_v1::HyprlandGlobalShortcutsManagerV1;

/// The app id the shortcuts are registered under, and the name of the submap
/// that binds keys to them.
pub const APP_ID: &str = "wl-wysiwyc";

/// A key the overlay acts on, named the way both xkb and the tool see it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Char(char),
    Escape,
    Backspace,
    Tab,
    LeftClick,
    RightClick,
    /// Start the overlay's choices over, without giving up on it.
    Reset,
    /// Move the target to the nearest one that way.
    Left,
    Right,
    Up,
    Down,
}

impl Key {
    /// The shortcut id, which is also the xkb key name the submap binds. The
    /// click keys are whatever the config says they are.
    pub fn name(self) -> String {
        match self {
            Key::Char(' ') => "space".into(),
            Key::Char(c) => c.to_string(),
            Key::Escape => "escape".into(),
            Key::Backspace => "backspace".into(),
            Key::Tab => "tab".into(),
            Key::LeftClick => crate::config::get().keys.left(),
            Key::RightClick => crate::config::get().keys.right(),
            Key::Reset => crate::config::get().keys.reset().unwrap_or_default(),
            Key::Left => "left".into(),
            Key::Right => "right".into(),
            Key::Up => "up".into(),
            Key::Down => "down".into(),
        }
    }
}

/// Every key the overlay listens for: hint letters, window numbers, the mode
/// toggle, the ones that back out, and the ones that click. A letter the
/// config gave to a click key is registered once, as that click key, so the
/// compositor is never asked to bind the same name twice.
pub fn keys() -> Vec<Key> {
    let reserved = crate::config::get().keys.reserved_letters();
    let mut out: Vec<Key> = ('a'..='z')
        .filter(|c| !reserved.contains(c))
        .map(Key::Char)
        .collect();
    out.extend(('1'..='9').map(Key::Char));
    out.extend([
        Key::Char(' '),
        Key::Escape,
        Key::Backspace,
        Key::Tab,
        Key::LeftClick,
        Key::RightClick,
        Key::Left,
        Key::Right,
        Key::Up,
        Key::Down,
    ]);
    // The key that opens the overlay is usually this one, and while the
    // overlay is up the compositor gives it to the submap rather than to the
    // keybind that started it. Binding it here is what makes a second press
    // mean something.
    if crate::config::get().keys.reset().is_some() {
        out.push(Key::Reset);
    }
    out
}

/// Holds the registered shortcuts and leaves the submap when dropped, so a
/// panic on the way out cannot stick the keyboard in a mode with no way back.
pub struct Shortcuts {
    manager: HyprlandGlobalShortcutsManagerV1,
    registered: Vec<HyprlandGlobalShortcutV1>,
    entered: bool,
}

impl Shortcuts {
    /// Register every key with the compositor and enter the submap that binds
    /// them. Returns None when the compositor does not speak the protocol, or
    /// when the submap could not be set up, so the caller can fall back to
    /// taking the keyboard the ordinary way.
    pub fn bind<D>(globals: &GlobalList, qh: &QueueHandle<D>) -> Option<Self>
    where
        D: Dispatch<HyprlandGlobalShortcutsManagerV1, ()>
            + Dispatch<HyprlandGlobalShortcutV1, Key>
            + 'static,
    {
        let manager: HyprlandGlobalShortcutsManagerV1 = globals.bind(qh, 1..=1, ()).ok()?;
        let registered = keys()
            .into_iter()
            .map(|key| {
                manager.register_shortcut(
                    key.name(),
                    APP_ID.to_string(),
                    format!("wl-wysiwyc {}", key.name()),
                    String::new(),
                    qh,
                    key,
                )
            })
            .collect();
        let mut shortcuts = Shortcuts {
            manager,
            registered,
            entered: false,
        };
        let binds: Vec<(String, String)> = keys()
            .into_iter()
            .map(|k| (k.name(), k.name()))
            .collect();
        if let Err(e) = hypr::enter_submap(&binds) {
            eprintln!("shortcuts: {e}");
            return None;
        }
        shortcuts.entered = true;
        Some(shortcuts)
    }
}

impl Drop for Shortcuts {
    fn drop(&mut self) {
        if self.entered {
            if let Err(e) = hypr::leave_submap() {
                eprintln!("shortcuts: {e}");
            }
        }
        for shortcut in self.registered.drain(..) {
            shortcut.destroy();
        }
        self.manager.destroy();
    }
}
