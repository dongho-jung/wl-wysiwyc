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
    pub label: Label,
    pub colors: Colors,
    pub elements: Elements,
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
    /// compositor wants. A letter here is kept out of the hints and the grid
    /// so it cannot mean two things at once.
    pub left_click: String,
    pub right_click: String,
    /// An extra key that clears everything typed and starts the overlay's
    /// choices over, the way Esc does before it gives up. Worth setting to
    /// whatever key opens the overlay: while the overlay is up that key
    /// belongs to it, so pressing it again undoes a wrong turn instead of
    /// doing nothing. Empty means no such key.
    pub reset: String,
    /// The keyboard the labels are laid out on, so that where an element is
    /// on screen decides which key names it.
    pub layout: Layout,
    /// Letters to keep out of hints and the grid, run together: `excluded:
    /// tyughvbn`. For the keys you would rather not have to reach for.
    pub excluded: String,
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
            left_click: "minus".into(),
            right_click: "equal".into(),
            reset: String::new(),
            layout: Layout::default(),
            excluded: String::new(),
        }
    }
}

impl Keys {
    /// The name the compositor knows a click key by. Punctuation is easier to
    /// write as itself than as `minus`, and in YAML a bare `-` means
    /// something else entirely, so both spellings are accepted.
    pub fn left(&self) -> String {
        key_name(&self.left_click)
    }

    pub fn right(&self) -> String {
        key_name(&self.right_click)
    }

    /// The reset key's name, if one is set.
    pub fn reset(&self) -> Option<String> {
        Some(key_name(&self.reset)).filter(|k| !k.is_empty())
    }

    /// The letters the click keys and the reset key have taken. They are
    /// still bound while the overlay is up, as those keys.
    pub fn taken_letters(&self) -> Vec<char> {
        [Some(self.left()), Some(self.right()), self.reset()]
            .into_iter()
            .flatten()
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
        "[" => "bracketleft",
        "]" => "bracketright",
        "`" => "grave",
        " " => "space",
        other => other,
    };
    named.to_string()
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
    /// Laid over the whole output so the labels have something to sit on.
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
    /// Grid tiles and the outline around the hinted window.
    pub tile: Hex,
    pub tile_border: Hex,
    /// Text on the dark parts: grid letters and window numbers.
    pub text: Hex,
}

impl Default for Colors {
    fn default() -> Self {
        Colors {
            dim: Hex(Color::new(0.0, 0.0, 0.0, 0.28)),
            shadow: Hex(Color::new(0.0, 0.0, 0.0, 0.42)),
            hint: Hex(Color::new(0.98, 0.79, 0.29, 0.88)),
            hint_text: Hex(Color::new(0.14, 0.09, 0.0, 1.0)),
            armed: Hex(Color::new(0.24, 0.85, 0.60, 0.92)),
            armed_text: Hex(Color::new(0.0, 0.14, 0.08, 1.0)),
            armed_key: Hex(Color::new(0.02, 0.24, 0.15, 0.92)),
            armed_key_text: Hex(Color::new(0.55, 1.0, 0.82, 1.0)),
            ring: Hex(Color::new(0.25, 0.92, 0.63, 0.95)),
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
        assert_eq!(c.keys.left(), "minus");
        assert_eq!(c.keys.right(), "equal");
        assert_eq!(c.keys.reset(), None);
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
        assert_eq!(c.keys.right(), "equal");
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
        assert_eq!(c.keys.left(), "minus");
        assert_eq!(c.keys.right(), "equal");
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
