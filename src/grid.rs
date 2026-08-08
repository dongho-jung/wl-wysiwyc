pub const ROWS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tile {
    pub ch: char,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Tiles covering a w x h rectangle, positions relative to its origin.
/// Three rows following the qwerty layout; each row splits the width
/// evenly among its letters. A letter that means something else elsewhere
/// keeps its place in the layout but gets no tile, so the geometry of the
/// remaining ones does not shift.
pub fn tiles(w: f64, h: f64) -> Vec<Tile> {
    let reserved = crate::config::get().keys.reserved_letters();
    let row_h = h / ROWS.len() as f64;
    let mut out = Vec::with_capacity(26);
    for (ri, row) in ROWS.iter().enumerate() {
        let cols = row.chars().count();
        let tile_w = w / cols as f64;
        for (ci, ch) in row.chars().enumerate() {
            if reserved.contains(&ch) {
                continue;
            }
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

    #[test]
    fn covers_all_letters_once() {
        let ts = tiles(100.0, 90.0);
        assert_eq!(ts.len(), 26);
        let mut chars: Vec<char> = ts.iter().map(|t| t.ch).collect();
        chars.sort_unstable();
        chars.dedup();
        assert_eq!(chars.len(), 26);
    }

    #[test]
    fn tiles_match_qwerty_geometry() {
        let ts = tiles(100.0, 90.0);
        let q = ts.iter().find(|t| t.ch == 'q').unwrap();
        assert_eq!((q.x, q.y, q.w, q.h), (0.0, 0.0, 10.0, 30.0));
        let a = ts.iter().find(|t| t.ch == 'a').unwrap();
        assert_eq!(a.y, 30.0);
        assert!((a.w - 100.0 / 9.0).abs() < 1e-9);
        let m = ts.iter().find(|t| t.ch == 'm').unwrap();
        assert_eq!(m.y, 60.0);
        assert!((m.x - 6.0 * (100.0 / 7.0)).abs() < 1e-9);
    }
}
