//! Settings read once from `~/.config/wl-wysiwyc/config.yaml`.
//!
//! Everything has a default, every field is optional, and a file that will
//! not parse is reported and then ignored: a typo in a colour should not
//! stop the overlay from opening.

use crate::draw::Color;
use serde::{Deserialize, Deserializer};
use std::path::PathBuf;
use std::sync::OnceLock;

static CONFIG: OnceLock<Config> = OnceLock::new();

/// The settings for this run. Loaded on first use.
pub fn get() -> &'static Config {
    CONFIG.get_or_init(load)
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_default();
    base.join("wl-wysiwyc").join("config.yaml")
}

fn load() -> Config {
    let path = path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Config::default(),
    };
    match serde_yaml::from_str(&text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config: {}: {e}", path.display());
            Config::default()
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub keys: Keys,
    pub click: Click,
    pub scroll: Scroll,
    pub pointer: Pointer,
    pub label: Label,
    pub colors: Colors,
    pub elements: Elements,
}

/// What holding a click key down does. A tap clicks once; keeping the key
/// down asks for the click twice, then three times, which is a steadier way
/// to double click than pressing anything twice in a row.
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Click {
    /// Hold at least this long for a double click, and this long for a
    /// triple. Zero on either turns that step off.
    pub double_ms: u64,
    pub triple_ms: u64,
    /// Fill the target as the hold goes on, so the count is something you
    /// watch rather than count out.
    pub charge: bool,
}

impl Default for Click {
    fn default() -> Self {
        Click {
            double_ms: 450,
            triple_ms: 1100,
            charge: true,
        }
    }
}

impl Click {
    /// How many clicks a hold of this long asks for.
    pub fn clicks(&self, held: std::time::Duration) -> u32 {
        let ms = held.as_millis() as u64;
        if self.triple_ms > 0 && ms >= self.triple_ms {
            3
        } else if self.double_ms > 0 && ms >= self.double_ms {
            2
        } else {
            1
        }
    }

    /// Which click a hold stands at, and how far along it is to the next
    /// one. The fill is drawn per step rather than over the whole hold, so
    /// that the two steps are told apart by watching one thing fill twice
    /// rather than by guessing at a fraction.
    pub fn stage(&self, held: std::time::Duration) -> (u32, f32) {
        let ms = held.as_millis() as f32;
        let (double, triple) = (self.double_ms as f32, self.triple_ms as f32);
        match self.clicks(held) {
            1 if double > 0.0 => (1, (ms / double).clamp(0.0, 1.0)),
            2 if triple > double => (2, ((ms - double) / (triple - double)).clamp(0.0, 1.0)),
            n => (n, 1.0),
        }
    }

    /// The whole hold, from press to the last step, which is what the fill
    /// measures itself against.
    pub fn span(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.triple_ms.max(self.double_ms))
    }

    /// How many clicks a hold can reach at all: one for the tap, and one for
    /// each step that is switched on.
    pub fn levels(&self) -> u32 {
        self.clicks(std::time::Duration::from_secs(3600))
    }
}

/// What the keys do.
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Keys {
    /// Ask for every key twice: the first press shows what it would select,
    /// the second takes it. Off by default, so one press is one key.
    pub confirm: bool,
    /// Click as soon as a hint is complete. Off by default: a complete hint
    /// puts the pointer on its target and waits, which leaves room to look
    /// before clicking and to pick which button.
    pub instant: bool,
    /// The keys that click. Either an xkb key name (`minus`, `return`,
    /// `space`) or the character itself, which is translated to the name the
    /// compositor wants, and either one key or a list of them. A letter here
    /// is kept out of the hints and the grid so it cannot mean two things at
    /// once.
    pub left_click: KeyList,
    pub right_click: KeyList,
    /// An extra key that clears everything typed and starts the overlay's
    /// choices over, the way Esc does before it gives up. Worth setting to
    /// whatever key opens the overlay: while the overlay is up that key
    /// belongs to it, so pressing it again undoes a wrong turn instead of
    /// doing nothing. Empty means no such key.
    pub reset: String,
    /// The keys that scroll the window under the pointer while the overlay
    /// is up. Shift with one scrolls to the end, ctrl scrolls sideways.
    /// Empty means no such key.
    pub scroll_up: String,
    pub scroll_down: String,
    /// The keyboard the labels are laid out on, so that where an element is
    /// on screen decides which key names it.
    pub layout: Layout,
    /// Letters to keep out of hints and the grid, run together: `excluded:
    /// tyughvbn`. For the keys you would rather not have to reach for.
    pub excluded: String,
    /// The key that swaps element hints for the letter grid. Empty means no
    /// such key, which is what giving space away to a click leaves behind.
    pub switch: String,
}

/// One key, or several. `left_click: minus` and `left_click: [minus, space]`
/// both parse; the first is the common case and should not have to be a list.
#[derive(Clone, Debug, Default)]
pub struct KeyList(pub Vec<String>);

impl<'de> Deserialize<'de> for KeyList {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(String),
            Many(Vec<String>),
        }
        Ok(KeyList(match OneOrMany::deserialize(d)? {
            OneOrMany::One(k) => vec![k],
            OneOrMany::Many(ks) => ks,
        }))
    }
}

/// Which keyboard the letters are arranged on.
#[derive(Deserialize, Default, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    #[default]
    Qwerty,
    Dvorak,
    /// No layout at all: labels are handed out a to z in reading order,
    /// which is the one thing that holds whatever keyboard you type on.
    None,
}

impl Default for Keys {
    fn default() -> Self {
        Keys {
            confirm: false,
            instant: false,
            // Enter costs nothing to give away: nothing else in the overlay
            // wants it. Space is not here for the opposite reason, since it
            // is what swaps hints for the grid.
            left_click: KeyList(vec!["minus".into(), "return".into()]),
            right_click: KeyList(vec!["equal".into()]),
            reset: String::new(),
            switch: "space".into(),
            scroll_up: "semicolon".into(),
            scroll_down: "apostrophe".into(),
            layout: Layout::default(),
            excluded: String::new(),
        }
    }
}

impl Keys {
    /// The names the compositor knows the click keys by. Punctuation is
    /// easier to write as itself than as `minus`, and in YAML a bare `-`
    /// means something else entirely, so both spellings are accepted.
    pub fn left(&self) -> Vec<String> {
        names(&self.left_click)
    }

    pub fn right(&self) -> Vec<String> {
        names(&self.right_click)
    }

    /// The reset key's name, if one is set.
    pub fn reset(&self) -> Option<String> {
        Some(key_name(&self.reset)).filter(|k| !k.is_empty())
    }

    /// The key that swaps hints for the grid, if it still has one.
    pub fn switch(&self) -> Option<String> {
        let name = key_name(&self.switch);
        // A key cannot both click and switch. Clicking wins, since that is
        // the one the config named twice on purpose.
        Some(name)
            .filter(|k| !k.is_empty())
            .filter(|k| !self.left().contains(k) && !self.right().contains(k))
    }

    /// The keys that scroll, if they are set.
    pub fn scroll_up(&self) -> Option<String> {
        Some(key_name(&self.scroll_up)).filter(|k| !k.is_empty())
    }

    pub fn scroll_down(&self) -> Option<String> {
        Some(key_name(&self.scroll_down)).filter(|k| !k.is_empty())
    }

    /// Every key name the overlay has already given a job to.
    pub fn claimed(&self) -> Vec<String> {
        let mut out = self.left();
        out.extend(self.right());
        out.extend(self.reset());
        out.extend(self.switch());
        out.extend(self.scroll_up());
        out.extend(self.scroll_down());
        out
    }

    /// The letters another key has taken. They are still bound while the
    /// overlay is up, as those keys.
    pub fn taken_letters(&self) -> Vec<char> {
        self.claimed()
            .into_iter()
            .filter_map(|k| {
                let mut cs = k.chars();
                match (cs.next(), cs.next()) {
                    (Some(c), None) if c.is_ascii_alphabetic() => Some(c.to_ascii_lowercase()),
                    _ => None,
                }
            })
            .collect()
    }

    /// Every letter a label may not use: the ones another key has taken, and
    /// the ones asked to be left out.
    pub fn reserved_letters(&self) -> Vec<char> {
        let mut out = self.taken_letters();
        out.extend(
            self.excluded
                .chars()
                .filter(|c| c.is_ascii_alphabetic())
                .map(|c| c.to_ascii_lowercase()),
        );
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// The xkb names for a list of keys, in the order they were written.
fn names(keys: &KeyList) -> Vec<String> {
    keys.0
        .iter()
        .map(|k| key_name(k))
        .filter(|k| !k.is_empty())
        .collect()
}

/// The xkb name for a key written as itself. Letters and digits are already
/// their own names; punctuation is not.
fn key_name(key: &str) -> String {
    let named = match key {
        "-" => "minus",
        "=" => "equal",
        ";" => "semicolon",
        "'" => "apostrophe",
        "," => "comma",
        "." => "period",
        "/" => "slash",
        "\\" => "backslash",
        "enter" => "return",
        "[" => "bracketleft",
        "]" => "bracketright",
        "`" => "grave",
        " " => "space",
        other => other,
    };
    named.to_string()
}

/// What the overlay does with the pointer.
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Pointer {
    /// How far the mouse has to be moved by hand before the overlay gets out
    /// of the way. Reaching for the mouse is saying the keyboard is not what
    /// you want after all, but a knock on the desk is not, so it takes a
    /// deliberate distance. Zero leaves the overlay up whatever the mouse
    /// does.
    pub cancel_px: f64,
    /// How hard an arrow key pushes the pointer, in pixels per second per
    /// second. It is flown rather than sent: a tap nudges it to the next
    /// thing along, holding builds speed across the window.
    pub accel_px: f64,
    /// What slows it down again, as a fraction of its speed per second.
    /// Together with the push this decides how fast it can go: about
    /// accel_px over drag.
    pub drag: f64,
    /// How near a target has to be to catch the pointer as it slows, and so
    /// how precisely it can be flown between two of them.
    pub snap_px: f64,
    /// About how long the pointer takes to reach a target. It is pulled
    /// there rather than placed, so this is the settling time of the pull,
    /// not a duration it is held to: a press while it is still moving adds
    /// to the speed it already has. Long enough to follow, short enough not
    /// to wait for.
    pub travel_ms: u64,
}

impl Default for Pointer {
    fn default() -> Self {
        Pointer {
            cancel_px: 24.0,
            accel_px: 5200.0,
            drag: 8.5,
            snap_px: 70.0,
            travel_ms: 200,
        }
    }
}

impl Pointer {
    /// The pull on the pointer, as the rate of the spring that moves it.
    /// Set from how long a trip should take: a damped spring is settled
    /// after about four and a half of these.
    pub fn spring(&self) -> f64 {
        4.6 / (self.travel_ms.max(20) as f64 / 1000.0)
    }
}

/// How much a scroll key moves the window under the pointer.
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scroll {
    /// Wheel notches per press, the way a mouse wheel counts them.
    pub step: u32,
    /// Notches for a press with shift, which is meant to reach the end of
    /// the document. Raise it for something longer than it gets to.
    pub far: u32,
    /// How long after the last scroll to read the window again. Everything
    /// has moved by then, so the hints have to be worked out afresh, and
    /// doing that on every press of a key held down would be a waste.
    pub settle_ms: u64,
}

impl Default for Scroll {
    fn default() -> Self {
        Scroll {
            step: 3,
            far: 200,
            settle_ms: 120,
        }
    }
}

impl Scroll {
    pub fn settle(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.settle_ms)
    }
}

/// Label geometry, in unscaled pixels. Everything here is multiplied by the
/// output's integer scale when drawn.
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Label {
    /// Text size.
    pub size: f32,
    /// Space left and right of the text, and above and below it.
    pub pad_x: f32,
    pub pad_y: f32,
    /// Clearance kept between one label and the next.
    pub gap: f32,
    /// Space between a label's own characters.
    pub track: f32,
}

impl Default for Label {
    fn default() -> Self {
        Label {
            size: 11.5,
            pad_x: 4.5,
            pad_y: 3.0,
            gap: 3.0,
            track: 2.5,
        }
    }
}

/// Colours, written as `#rgb`, `#rrggbb` or `#rrggbbaa`.
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Colors {
    /// Laid over the whole output. Transparent by default: the labels carry
    /// themselves, and darkening the screen to read them is a tax on every
    /// glance. Give it some alpha to have them stand out more.
    pub dim: Hex,
    /// Under every label and panel.
    pub shadow: Hex,
    /// A hint waiting to be typed.
    pub hint: Hex,
    pub hint_text: Hex,
    /// A hint the armed key would keep.
    pub armed: Hex,
    pub armed_text: Hex,
    /// The armed key itself, shown pressed inside those hints.
    pub armed_key: Hex,
    pub armed_key_text: Hex,
    /// Around the element a further press would click.
    pub ring: Hex,
    /// The fill that runs while a click key is held.
    pub charge: Hex,
    /// Grid tiles and the outline around the hinted window.
    pub tile: Hex,
    pub tile_border: Hex,
    /// Text on the dark parts: grid letters and window numbers.
    pub text: Hex,
}

impl Default for Colors {
    fn default() -> Self {
        Colors {
            dim: Hex(Color::new(0.0, 0.0, 0.0, 0.0)),
            shadow: Hex(Color::new(0.0, 0.0, 0.0, 0.42)),
            hint: Hex(Color::new(0.98, 0.79, 0.29, 0.88)),
            hint_text: Hex(Color::new(0.14, 0.09, 0.0, 1.0)),
            armed: Hex(Color::new(0.24, 0.85, 0.60, 0.92)),
            armed_text: Hex(Color::new(0.0, 0.14, 0.08, 1.0)),
            armed_key: Hex(Color::new(0.02, 0.24, 0.15, 0.92)),
            armed_key_text: Hex(Color::new(0.55, 1.0, 0.82, 1.0)),
            ring: Hex(Color::new(0.25, 0.92, 0.63, 0.95)),
            charge: Hex(Color::new(0.98, 0.79, 0.29, 0.95)),
            tile: Hex(Color::new(0.08, 0.08, 0.10, 0.20)),
            tile_border: Hex(Color::new(1.0, 1.0, 1.0, 0.30)),
            text: Hex(Color::new(1.0, 1.0, 1.0, 0.96)),
        }
    }
}

/// Limits on reading a window's accessibility tree.
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Elements {
    /// Stop after this many clickable elements.
    pub max: usize,
    /// Give up walking the tree after this long.
    pub walk_ms: u64,
    /// Hard limit on the whole query, including a toolkit that never answers.
    pub query_ms: u64,
}

impl Default for Elements {
    fn default() -> Self {
        Elements {
            max: 400,
            walk_ms: 1200,
            query_ms: 1800,
        }
    }
}

/// A colour written as a hex string.
#[derive(Clone, Copy)]
pub struct Hex(pub Color);

impl std::ops::Deref for Hex {
    type Target = Color;
    fn deref(&self) -> &Color {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Hex {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        parse_hex(&s)
            .map(Hex)
            .ok_or_else(|| serde::de::Error::custom(format!("not a colour: {s}")))
    }
}

/// `#rgb`, `#rrggbb` and `#rrggbbaa`, with or without the hash.
fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    let nibble = |i: usize| u8::from_str_radix(s.get(i..i + 1)?, 16).ok();
    let byte = |i: usize| u8::from_str_radix(s.get(i..i + 2)?, 16).ok();
    let v = |b: u8| b as f32 / 255.0;
    match s.len() {
        3 => Some(Color::new(
            v(nibble(0)? * 17),
            v(nibble(1)? * 17),
            v(nibble(2)? * 17),
            1.0,
        )),
        6 => Some(Color::new(v(byte(0)?), v(byte(2)?), v(byte(4)?), 1.0)),
        8 => Some(Color::new(
            v(byte(0)?),
            v(byte(2)?),
            v(byte(4)?),
            v(byte(6)?),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_read_in_every_length() {
        let red = parse_hex("#f00").unwrap();
        assert_eq!((red.r, red.g, red.b, red.a), (1.0, 0.0, 0.0, 1.0));
        let green = parse_hex("00ff00").unwrap();
        assert_eq!((green.r, green.g, green.b, green.a), (0.0, 1.0, 0.0, 1.0));
        let half = parse_hex("#00000080").unwrap();
        assert!((half.a - 0.502).abs() < 0.01);
        assert!(parse_hex("#12345").is_none());
        assert!(parse_hex("nope").is_none());
    }

    #[test]
    fn an_empty_file_is_all_defaults() {
        let c: Config = serde_yaml::from_str("{}").unwrap();
        assert!(!c.keys.confirm);
        assert!(!c.keys.instant);
        assert_eq!(c.keys.left(), ["minus", "return"]);
        assert_eq!(c.keys.right(), ["equal"]);
        assert_eq!(c.keys.reset(), None);
        assert_eq!(c.keys.switch().as_deref(), Some("space"));
        assert_eq!(c.label.size, 11.5);
        assert_eq!(c.elements.max, 400);
    }

    #[test]
    fn a_partial_file_keeps_the_rest() {
        let c: Config = serde_yaml::from_str(
            "keys:\n  confirm: true\nlabel:\n  size: 20\ncolors:\n  hint: \"#123456\"\n",
        )
        .unwrap();
        assert!(c.keys.confirm);
        assert_eq!(c.keys.right(), ["equal"]);
        assert_eq!(c.label.size, 20.0);
        assert_eq!(c.label.pad_x, 4.5);
        assert!((c.colors.hint.r - 0.07).abs() < 0.01);
        assert_eq!(c.colors.armed_text.a, 1.0);
    }

    #[test]
    fn a_click_key_that_is_a_letter_is_reserved() {
        let c: Config =
            serde_yaml::from_str("keys:\n  left_click: f\n  right_click: semicolon\n").unwrap();
        assert_eq!(c.keys.reserved_letters(), vec!['f']);
        assert_eq!(c.keys.taken_letters(), vec!['f']);
        assert!(Keys::default().reserved_letters().is_empty());
    }

    #[test]
    fn punctuation_is_named_either_way() {
        let c: Config =
            serde_yaml::from_str("keys:\n  left_click: \"-\"\n  right_click: equal\n").unwrap();
        assert_eq!(c.keys.left(), ["minus"]);
        assert_eq!(c.keys.right(), ["equal"]);
        assert!(c.keys.reserved_letters().is_empty());
    }

    #[test]
    fn excluded_letters_join_the_reserved_ones() {
        let c: Config =
            serde_yaml::from_str("keys:\n  left_click: f\n  excluded: TYUghvbn\n").unwrap();
        assert_eq!(
            c.keys.reserved_letters(),
            vec!['b', 'f', 'g', 'h', 'n', 't', 'u', 'v', 'y']
        );
        // Excluded keys are still bound while the overlay is up, so they do
        // nothing rather than reaching the window underneath.
        assert_eq!(c.keys.taken_letters(), vec!['f']);
    }

    #[test]
    fn the_scroll_keys_are_claimed_like_any_other() {
        let c: Config = serde_yaml::from_str("keys:\n  scroll_up: \";\"\n").unwrap();
        assert_eq!(c.keys.scroll_up().as_deref(), Some("semicolon"));
        assert_eq!(c.keys.scroll_down().as_deref(), Some("apostrophe"));
        assert!(c.keys.claimed().contains(&"semicolon".to_string()));
        let c: Config =
            serde_yaml::from_str("keys:\n  scroll_up: j\n  scroll_down: \"\"\n").unwrap();
        assert_eq!(c.keys.scroll_down(), None);
        assert!(c.keys.reserved_letters().contains(&'j'));
    }

    #[test]
    fn a_hold_asks_for_more_clicks_the_longer_it_is() {
        let c = Click::default();
        let ms = std::time::Duration::from_millis;
        assert_eq!(c.clicks(ms(0)), 1);
        assert_eq!(c.clicks(ms(449)), 1);
        assert_eq!(c.clicks(ms(450)), 2);
        assert_eq!(c.clicks(ms(1099)), 2);
        assert_eq!(c.clicks(ms(1100)), 3);
        assert_eq!(c.clicks(ms(5000)), 3);
        assert_eq!(c.span(), ms(1100));
        // Each step fills on its own, and the last one sits full.
        assert_eq!(c.stage(ms(0)), (1, 0.0));
        assert_eq!(c.stage(ms(225)), (1, 0.5));
        assert_eq!(c.stage(ms(450)), (2, 0.0));
        assert_eq!(c.stage(ms(775)), (2, 0.5));
        assert_eq!(c.stage(ms(1100)), (3, 1.0));
        assert_eq!(c.stage(ms(9000)), (3, 1.0));

        // Either step off means a hold never reaches it.
        let single = Click {
            double_ms: 0,
            triple_ms: 0,
            charge: false,
        };
        assert_eq!(single.clicks(ms(9000)), 1);
        assert_eq!(single.levels(), 1);
        assert_eq!(Click::default().levels(), 3);
        let no_triple: Click = serde_yaml::from_str("triple_ms: 0").unwrap();
        assert_eq!(no_triple.clicks(ms(9000)), 2);
        assert_eq!(no_triple.span(), ms(450));
        assert_eq!(no_triple.stage(ms(9000)), (2, 1.0));
    }

    #[test]
    fn a_click_takes_one_key_or_several() {
        let c: Config = serde_yaml::from_str(
            "keys:\n  left_click: [\"-\", space, enter]\n  right_click: \"=\"\n",
        )
        .unwrap();
        assert_eq!(c.keys.left(), ["minus", "space", "return"]);
        assert_eq!(c.keys.right(), ["equal"]);
        // Space clicks now, so it cannot also swap hints for the grid.
        assert_eq!(c.keys.switch(), None);
    }

    #[test]
    fn a_layout_is_named_in_lower_case() {
        let c: Config = serde_yaml::from_str("keys:\n  layout: dvorak\n").unwrap();
        assert_eq!(c.keys.layout, Layout::Dvorak);
        let c: Config = serde_yaml::from_str("keys:\n  layout: none\n").unwrap();
        assert_eq!(c.keys.layout, Layout::None);
        assert_eq!(Keys::default().layout, Layout::Qwerty);
        assert!(serde_yaml::from_str::<Config>("keys:\n  layout: azerty\n").is_err());
    }

    #[test]
    fn an_unknown_key_is_an_error_rather_than_a_shrug() {
        assert!(serde_yaml::from_str::<Config>("colour: red\n").is_err());
    }
}
