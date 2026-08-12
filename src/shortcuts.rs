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

/// One of the four directions keyboard navigation can move.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arrow {
    Left,
    Right,
    Up,
    Down,
}

impl Arrow {
    pub const EVERY: [Arrow; 4] = [Arrow::Left, Arrow::Right, Arrow::Up, Arrow::Down];

    pub(crate) fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Up => 2,
            Self::Down => 3,
        }
    }

    pub(crate) fn vector(self) -> (f64, f64) {
        match self {
            Self::Left => (-1.0, 0.0),
            Self::Right => (1.0, 0.0),
            Self::Up => (0.0, -1.0),
            Self::Down => (0.0, 1.0),
        }
    }

    pub(crate) fn wheel(self) -> Wheel {
        match self {
            Self::Left => Wheel {
                back: true,
                across: true,
            },
            Self::Right => Wheel {
                back: false,
                across: true,
            },
            Self::Up => Wheel {
                back: true,
                across: false,
            },
            Self::Down => Wheel {
                back: false,
                across: false,
            },
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

/// How an arrow moves. The modifier is encoded in the compositor binding,
/// so every mode arrives as its own shortcut without taking keyboard focus.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NavMode {
    #[default]
    Normal,
    Free,
    Instant,
}

impl NavMode {
    fn modifiers(self) -> &'static [&'static str] {
        match self {
            Self::Normal => &[""],
            Self::Free => &["ALT", "SHIFT + ALT"],
            Self::Instant => &["CTRL", "CTRL + SHIFT", "CTRL + ALT", "CTRL + SHIFT + ALT"],
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Normal => "",
            Self::Free => "free-",
            Self::Instant => "instant-",
        }
    }
}

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
    /// Move the pointer in one of the three navigation modes.
    Arrow(Arrow, NavMode),
    /// Scroll the window under the pointer with a configured vertical key.
    Scroll(Wheel),
    /// Scroll in any direction with Shift and the matching arrow.
    ShiftScroll(Wheel),
}

impl Key {
    pub(crate) fn is_motion_arrow(self) -> bool {
        matches!(self, Self::Arrow(_, NavMode::Normal | NavMode::Free))
    }

    pub(crate) fn repeats(self) -> bool {
        self.is_motion_arrow() || matches!(self, Self::ShiftScroll(_))
    }

    pub(crate) fn transparent(self) -> bool {
        matches!(self, Self::Arrow(..) | Self::ShiftScroll(_))
    }
}

/// A scroll direction encoded as a wheel axis and sign.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Wheel {
    /// Up, or left when across.
    pub back: bool,
    /// Sideways.
    pub across: bool,
}

impl Wheel {
    fn way(self) -> &'static str {
        match (self.across, self.back) {
            (false, true) => "up",
            (false, false) => "down",
            (true, true) => "left",
            (true, false) => "right",
        }
    }
}

/// The modifier combinations a click key answers to, so that shift, ctrl and
/// alt reach the window instead of stopping at the overlay.
///
/// Nothing is done with them here. The overlay never takes the keyboard, so
/// the window under it already knows which modifiers are down; injecting the
/// click while they are held is all a shift click is. Binding them only stops
/// the compositor handing the combination back to the window as a keystroke.
/// Each modifier is its own term: the compositor reads what is between the
/// pluses one at a time and does not know "CTRL SHIFT" as a name.
const MODS: [&str; 8] = [
    "",
    "SHIFT",
    "CTRL",
    "ALT",
    "CTRL + SHIFT",
    "CTRL + ALT",
    "SHIFT + ALT",
    "CTRL + SHIFT + ALT",
];

impl Key {
    /// The keys the submap presses this shortcut with, which is usually just
    /// the key's own name. A click answers to every key the config gave it,
    /// each with every modifier combination, so a shortcut is not always one
    /// binding.
    pub fn bindings(self) -> Vec<String> {
        if let Key::Arrow(way, mode) = self {
            return mode
                .modifiers()
                .iter()
                .map(|modifier| match modifier.is_empty() {
                    true => way.name().into(),
                    false => format!("{modifier} + {}", way.name()),
                })
                .collect();
        }
        if let Key::ShiftScroll(w) = self {
            return vec![format!("SHIFT + {}", w.way())];
        }
        let keys = &crate::config::get().keys;
        if let Key::Scroll(w) = self {
            if w.across {
                return Vec::new();
            }
            return match (w.back, keys.scroll_up(), keys.scroll_down()) {
                (true, Some(key), _) | (false, _, Some(key)) => vec![key],
                _ => Vec::new(),
            };
        }
        let clicks = match self {
            Key::LeftClick => keys.left(),
            Key::RightClick => keys.right(),
            other => return vec![other.name()],
        };
        clicks
            .iter()
            .flat_map(|k| {
                MODS.iter().map(move |m| match *m {
                    "" => k.clone(),
                    mods => format!("{mods} + {k}"),
                })
            })
            .collect()
    }

    /// The shortcut id. Plain keys use their xkb name, while modified arrows
    /// carry their movement mode so each global shortcut remains distinct. A
    /// click key answering to several keys is registered once, under the
    /// first of them.
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
            Key::Arrow(way, mode) => format!("{}{name}", mode.id(), name = way.name()),
            Key::Scroll(w) => format!("scroll-{}", w.way()),
            Key::ShiftScroll(w) => format!("shift-scroll-{}", w.way()),
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
        Key::Escape,
        Key::Backspace,
        Key::Tab,
    ];
    out.extend(
        [NavMode::Normal, NavMode::Free, NavMode::Instant]
            .into_iter()
            .flat_map(|mode| {
                Arrow::EVERY
                    .into_iter()
                    .map(move |way| Key::Arrow(way, mode))
            }),
    );
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
        Arrow::EVERY
            .into_iter()
            .map(|way| Key::ShiftScroll(way.wheel())),
    );
    out.extend(
        [Arrow::Up, Arrow::Down]
            .into_iter()
            .map(|way| Key::Scroll(way.wheel())),
    );
    out.extend(
        ('a'..='z')
            .chain('1'..='9')
            .chain([' '])
            .map(Key::Char)
            .filter(|k| !claimed.contains(&k.name())),
    );
    out.retain(|k| !k.name().is_empty() && !k.bindings().is_empty());
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
        let mut registered = Vec::new();
        for key in keys() {
            let name = key.name();
            registered.push(manager.register_shortcut(
                name.clone(),
                APP_ID.to_string(),
                format!("wl-wysiwyc {name}"),
                String::new(),
                qh,
                key,
            ));
        }
        let mut shortcuts = Shortcuts {
            manager,
            registered,
            entered: false,
        };
        let binds: Vec<(String, String, bool, bool)> = keys()
            .into_iter()
            .flat_map(|k| {
                let id = k.name();
                let repeat = k.repeats();
                let transparent = k.transparent();
                k.bindings()
                    .into_iter()
                    .map(move |b| (id.clone(), b, repeat, transparent))
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

#[cfg(test)]
mod tests {
    use super::{keys, Arrow, Key, NavMode};

    #[test]
    fn every_arrow_modifier_has_a_distinct_binding() {
        let cases: [(NavMode, &str, &[&str], bool); 3] = [
            (NavMode::Normal, "left", &["left"], true),
            (
                NavMode::Free,
                "free-left",
                &["ALT + left", "SHIFT + ALT + left"],
                true,
            ),
            (
                NavMode::Instant,
                "instant-left",
                &[
                    "CTRL + left",
                    "CTRL + SHIFT + left",
                    "CTRL + ALT + left",
                    "CTRL + SHIFT + ALT + left",
                ],
                false,
            ),
        ];
        for (mode, id, bindings, repeats) in cases {
            let key = Key::Arrow(Arrow::Left, mode);
            assert_eq!(key.name(), id);
            assert_eq!(key.bindings(), bindings);
            assert_eq!(key.repeats(), repeats);
            assert!(key.transparent());
        }
    }

    #[test]
    fn shift_arrows_are_repeating_directional_scrolls() {
        let key = Key::ShiftScroll(Arrow::Left.wheel());
        assert_eq!(key.name(), "shift-scroll-left");
        assert_eq!(key.bindings(), ["SHIFT + left"]);
        assert!(key.repeats());
        assert!(key.transparent());
    }

    #[test]
    fn dedicated_vertical_scroll_keys_keep_the_old_defaults() {
        let up = Key::Scroll(Arrow::Up.wheel());
        let down = Key::Scroll(Arrow::Down.wheel());
        assert_eq!(up.name(), "scroll-up");
        assert_eq!(down.name(), "scroll-down");
        assert_eq!(up.bindings(), ["semicolon"]);
        assert_eq!(down.bindings(), ["apostrophe"]);
        assert!(!up.repeats());
        assert!(!up.transparent());
    }

    #[test]
    fn the_shortcut_set_contains_navigation_and_scroll_directions() {
        let keys = keys();
        assert_eq!(
            keys.iter()
                .filter(|key| matches!(key, Key::Arrow(..)))
                .count(),
            12
        );
        assert_eq!(
            keys.iter()
                .filter(|key| matches!(key, Key::ShiftScroll(_)))
                .count(),
            4
        );
        assert_eq!(
            keys.iter()
                .filter(|key| matches!(key, Key::Scroll(_)))
                .count(),
            2
        );
    }
}
