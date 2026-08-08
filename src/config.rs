//! Settings read once from `~/.config/wl-wysiwyc/config.yaml`.
//!
//! Everything has a default, every field is optional, and a file that will
//! not parse is reported and then ignored: a typo in a colour should not
//! stop the overlay from opening.

use crate::draw::Color;
use serde::{Deserialize, Deserializer};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

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
    /// How long an armed key waits before confirming itself, in
    /// milliseconds. Zero means it never does, so every key needs its second
    /// press.
    pub confirm_ms: ConfirmMs,
    pub label: Label,
    pub colors: Colors,
    pub elements: Elements,
}

impl Config {
    pub fn confirm_delay(&self) -> Option<Duration> {
        match self.confirm_ms.0 {
            0 => None,
            ms => Some(Duration::from_millis(ms)),
        }
    }
}

pub struct ConfirmMs(pub u64);

impl Default for ConfirmMs {
    fn default() -> Self {
        ConfirmMs(300)
    }
}

impl<'de> Deserialize<'de> for ConfirmMs {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        u64::deserialize(d).map(ConfirmMs)
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
            hint: Hex(Color::new(0.98, 0.79, 0.29, 0.97)),
            hint_text: Hex(Color::new(0.14, 0.09, 0.0, 1.0)),
            armed: Hex(Color::new(0.24, 0.85, 0.60, 0.98)),
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
        assert_eq!(c.confirm_ms.0, 300);
        assert_eq!(c.label.size, 11.5);
        assert_eq!(c.elements.max, 400);
    }

    #[test]
    fn a_partial_file_keeps_the_rest() {
        let c: Config = serde_yaml::from_str(
            "confirm_ms: 0\nlabel:\n  size: 20\ncolors:\n  hint: \"#123456\"\n",
        )
        .unwrap();
        assert!(c.confirm_delay().is_none());
        assert_eq!(c.label.size, 20.0);
        assert_eq!(c.label.pad_x, 4.5);
        assert!((c.colors.hint.r - 0.07).abs() < 0.01);
        assert_eq!(c.colors.armed_text.a, 1.0);
    }

    #[test]
    fn an_unknown_key_is_an_error_rather_than_a_shrug() {
        assert!(serde_yaml::from_str::<Config>("colour: red\n").is_err());
    }
}
