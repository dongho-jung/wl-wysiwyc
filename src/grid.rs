use crate::config::Layout;

/// The three rows of letters a layout puts under your fingers, in the order
/// they sit there. Only letters: a hint made of punctuation would be worse to
/// type than the pointer it saves.
pub fn rows_for(layout: Layout) -> &'static [&'static str; 3] {
    match layout {
        Layout::Qwerty => &["qwertyuiop", "asdfghjkl", "zxcvbnm"],
        // Dvorak's top row starts with three punctuation keys, so its letters
        // begin at the fourth column; the same is true of qwerty's bottom row
        // and is already spread across the width the same way.
        Layout::Dvorak => &["pyfgcrl", "aoeuidhtns", "qjkxbmwvz"],
        // Nothing to follow, so the alphabet in reading order.
        Layout::None => &["abcdefghij", "klmnopqrst", "uvwxyz"],
    }
}

/// The rows this run is using.
pub fn rows() -> &'static [&'static str; 3] {
    rows_for(crate::config::get().keys.layout)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tile {
    pub ch: char,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Tiles covering a w x h rectangle, positions relative to its origin.
///
/// Three rows following the keyboard; each row splits the width evenly among
/// the letters it has left. A letter that means something else, or one the
/// config asked to leave out, is dropped from its row and the rest spread to
/// fill the gap. The grid is the fallback for windows with nothing to hint,
/// and a grid with holes in it cannot reach part of the window at all, which
/// matters more here than the letters keeping their exact place.
pub fn tiles(w: f64, h: f64) -> Vec<Tile> {
    tiles_on(rows(), &crate::config::get().keys.reserved_letters(), w, h)
}

fn tiles_on(rows: &[&str; 3], reserved: &[char], w: f64, h: f64) -> Vec<Tile> {
    let row_h = h / rows.len() as f64;
    let mut out = Vec::with_capacity(26);
    for (ri, row) in rows.iter().enumerate() {
        let letters: Vec<char> = row.chars().filter(|c| !reserved.contains(c)).collect();
        if letters.is_empty() {
            continue;
        }
        let tile_w = w / letters.len() as f64;
        for (ci, ch) in letters.into_iter().enumerate() {
            out.push(Tile {
                ch,
                x: ci as f64 * tile_w,
                y: ri as f64 * row_h,
                w: tile_w,
                h: row_h,
            });
        }
    }
    out
}

pub fn tile_for(w: f64, h: f64, ch: char) -> Option<Tile> {
    tiles(w, h).into_iter().find(|t| t.ch == ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qwerty() -> &'static [&'static str; 3] {
        rows_for(Layout::Qwerty)
    }

    #[test]
    fn covers_all_letters_once() {
        let ts = tiles_on(qwerty(), &[], 100.0, 90.0);
        assert_eq!(ts.len(), 26);
        let mut chars: Vec<char> = ts.iter().map(|t| t.ch).collect();
        chars.sort_unstable();
        chars.dedup();
        assert_eq!(chars.len(), 26);
    }

    #[test]
    fn every_layout_covers_the_alphabet() {
        for layout in [Layout::Qwerty, Layout::Dvorak, Layout::None] {
            let mut chars: Vec<char> = rows_for(layout).iter().flat_map(|r| r.chars()).collect();
            chars.sort_unstable();
            chars.dedup();
            assert_eq!(chars.len(), 26, "{layout:?} does not spell the alphabet");
        }
    }

    #[test]
    fn tiles_match_qwerty_geometry() {
        let ts = tiles_on(qwerty(), &[], 100.0, 90.0);
        let q = ts.iter().find(|t| t.ch == 'q').unwrap();
        assert_eq!((q.x, q.y, q.w, q.h), (0.0, 0.0, 10.0, 30.0));
        let a = ts.iter().find(|t| t.ch == 'a').unwrap();
        assert_eq!(a.y, 30.0);
        assert!((a.w - 100.0 / 9.0).abs() < 1e-9);
        let m = ts.iter().find(|t| t.ch == 'm').unwrap();
        assert_eq!(m.y, 60.0);
        assert!((m.x - 6.0 * (100.0 / 7.0)).abs() < 1e-9);
    }

    #[test]
    fn a_row_that_lost_letters_still_covers_its_width() {
        let ts = tiles_on(qwerty(), &"tyu".chars().collect::<Vec<_>>(), 100.0, 90.0);
        let top: Vec<&Tile> = ts.iter().filter(|t| t.y == 0.0).collect();
        assert_eq!(top.len(), 7);
        assert!(top.iter().all(|t| (t.w - 100.0 / 7.0).abs() < 1e-9));
        let last = top.last().unwrap();
        assert!((last.x + last.w - 100.0).abs() < 1e-9);
        assert!(!ts.iter().any(|t| "tyu".contains(t.ch)));
    }
}
