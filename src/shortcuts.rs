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
    /// Swap element hints for the letter grid, and back.
    Switch,
    /// Move the target to the nearest one that way.
    Left,
    Right,
    Up,
    Down,
    /// Shift and the left click key together: click twice.
    DoubleClick,
}

impl Key {
    /// The keys the submap presses this shortcut with, which is usually just
    /// the key's own name. A click can answer to several keys, and a double
    /// click is any of the left click keys with shift held, so a shortcut is
    /// not always one binding.
    pub fn bindings(self) -> Vec<String> {
        let keys = &crate::config::get().keys;
        match self {
            Key::LeftClick => keys.left(),
            Key::RightClick => keys.right(),
            Key::DoubleClick => keys.left().iter().map(|k| format!("SHIFT + {k}")).collect(),
            other => vec![other.name()],
        }
    }

    /// The shortcut id, which for everything but the click keys is also the
    /// xkb key name the submap binds. A click key answering to several keys
    /// is registered once, under the first of them.
    pub fn name(self) -> String {
        let keys = &crate::config::get().keys;
        let first = |ks: Vec<String>| ks.into_iter().next().unwrap_or_default();
        match self {
            Key::Char(' ') => "space".into(),
            Key::Char(c) => c.to_string(),
            Key::Escape => "escape".into(),
            Key::Backspace => "backspace".into(),
            Key::Tab => "tab".into(),
            Key::LeftClick => first(keys.left()),
            Key::RightClick => first(keys.right()),
            Key::Reset => keys.reset().unwrap_or_default(),
            Key::Switch => keys.switch().unwrap_or_default(),
            Key::Left => "left".into(),
            Key::Right => "right".into(),
            Key::Up => "up".into(),
            Key::Down => "down".into(),
            Key::DoubleClick => "double".into(),
        }
    }
}

/// Every key the overlay listens for: hint letters, window numbers, the mode
/// toggle, the ones that back out, and the ones that click. A key the config
/// gave a job to is registered once, for that job, so the compositor is never
/// asked to bind the same name twice. An excluded letter is still bound, and
/// still does nothing: a key the submap has no bind for falls through to the
/// window underneath, which would type into it.
pub fn keys() -> Vec<Key> {
    let cfg = &crate::config::get().keys;
    let claimed = cfg.claimed();
    let mut out = vec![
        Key::LeftClick,
        Key::RightClick,
        Key::DoubleClick,
        Key::Escape,
        Key::Backspace,
        Key::Tab,
        Key::Left,
        Key::Right,
        Key::Up,
        Key::Down,
    ];
    // The key that opens the overlay is usually the reset key, and while the
    // overlay is up the compositor gives it to the submap rather than to the
    // keybind that started it. Binding it here is what makes a second press
    // mean something.
    if cfg.reset().is_some() {
        out.push(Key::Reset);
    }
    if cfg.switch().is_some() {
        out.push(Key::Switch);
    }
    out.extend(
        ('a'..='z')
            .chain('1'..='9')
            .chain([' '])
            .map(Key::Char)
            .filter(|k| !claimed.contains(&k.name())),
    );
    out.retain(|k| !k.name().is_empty());
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
            .flat_map(|k| {
                let id = k.name();
                k.bindings().into_iter().map(move |b| (id.clone(), b))
            })
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
