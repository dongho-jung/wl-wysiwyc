use crate::grid::ROWS;

/// One key of the qwerty block, with its center normalized on both axes so
/// key positions and element positions can be compared directly.
struct Key {
    ch: char,
    x: f64,
    y: f64,
}

fn keys() -> Vec<Key> {
    let rows = ROWS.len() as f64;
    let mut out = Vec::with_capacity(26);
    for (r, row) in ROWS.iter().enumerate() {
        let cols = row.chars().count() as f64;
        for (c, ch) in row.chars().enumerate() {
            out.push(Key {
                ch,
                x: (c as f64 + 0.5) / cols,
                y: (r as f64 + 0.5) / rows,
            });
        }
    }
    out
}

/// The key whose cell covers a normalized position. Rows split the height
/// evenly, and each row splits the width among its own letters, so the
/// keyboard is laid over the elements the same way the grid is laid over a
/// window.
fn key_at(x: f64, y: f64) -> usize {
    let r = ((y.clamp(0.0, 1.0) * ROWS.len() as f64) as usize).min(ROWS.len() - 1);
    let cols = ROWS[r].chars().count();
    let c = ((x.clamp(0.0, 1.0) * cols as f64) as usize).min(cols - 1);
    ROWS[..r].iter().map(|s| s.chars().count()).sum::<usize>() + c
}

/// Hand out keys to points, at most `capacity` points per key: the key
/// covering the point when it still has room, otherwise the nearest key that
/// does. Points are served top-to-bottom so a crowded corner spills in
/// reading order instead of at random.
fn spread(points: &[(f64, f64)], capacity: usize) -> Vec<usize> {
    let keys = keys();
    let mut used = vec![0usize; keys.len()];
    let mut out = vec![0usize; points.len()];
    let mut order: Vec<usize> = (0..points.len()).collect();
    order.sort_by(|&a, &b| {
        points[a]
            .1
            .total_cmp(&points[b].1)
            .then(points[a].0.total_cmp(&points[b].0))
    });
    for i in order {
        let (x, y) = points[i];
        let want = key_at(x, y);
        let k = if used[want] < capacity {
            want
        } else {
            let d2 = |k: &Key| (k.x - x).powi(2) + (k.y - y).powi(2);
            (0..keys.len())
                .filter(|&k| used[k] < capacity)
                .min_by(|&a, &b| d2(&keys[a]).total_cmp(&d2(&keys[b])))
                .unwrap_or(want)
        };
        used[k] += 1;
        out[i] = k;
    }
    out
}

/// Positions of a group within its own bounding box, both axes on [0, 1].
/// A group with no extent along an axis sits in the middle of it.
fn normalize(centers: &[(f64, f64)], group: &[usize]) -> Vec<(f64, f64)> {
    let (mut x0, mut y0) = (f64::INFINITY, f64::INFINITY);
    let (mut x1, mut y1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for &i in group {
        let (x, y) = centers[i];
        x0 = x0.min(x);
        x1 = x1.max(x);
        y0 = y0.min(y);
        y1 = y1.max(y);
    }
    let (w, h) = (x1 - x0, y1 - y0);
    group
        .iter()
        .map(|&i| {
            let (x, y) = centers[i];
            let nx = if w > 0.0 { (x - x0) / w } else { 0.5 };
            let ny = if h > 0.0 { (y - y0) / h } else { 0.5 };
            (nx, ny)
        })
        .collect()
}

fn assign(centers: &[(f64, f64)], group: &[usize], prefix: &str, out: &mut [String]) {
    if group.is_empty() {
        return;
    }
    if group.len() == 1 && !prefix.is_empty() {
        out[group[0]] = prefix.to_string();
        return;
    }
    let keys = keys();
    // One key per element while 26 of them suffice. Past that the key becomes
    // a prefix and the group it collected is labelled the same way again, so
    // labels grow by a character only where elements are actually crowded.
    let capacity = group.len().div_ceil(keys.len()).max(1);
    let picked = spread(&normalize(centers, group), capacity);
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); keys.len()];
    for (&k, &i) in picked.iter().zip(group) {
        buckets[k].push(i);
    }
    for (k, bucket) in buckets.iter().enumerate() {
        assign(centers, bucket, &format!("{prefix}{}", keys[k].ch), out);
    }
}

/// Labels for elements at the given centers, returned in the same order.
/// Position picks the key: an element in the top-left corner is labelled
/// near Q, one in the bottom-right near M, so the label a target gets
/// follows from where it sits. Labels are prefix-free, so a complete label
/// is never the start of another one.
pub fn labels(centers: &[(f64, f64)]) -> Vec<String> {
    let mut out = vec![String::new(); centers.len()];
    let all: Vec<usize> = (0..centers.len()).collect();
    assign(centers, &all, "", &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Centers of a cols x rows arrangement over a 1000 x 1000 window,
    /// in reading order.
    fn lattice(cols: usize, rows: usize) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        for r in 0..rows {
            for c in 0..cols {
                out.push((
                    (c as f64 + 0.5) * 1000.0 / cols as f64,
                    (r as f64 + 0.5) * 1000.0 / rows as f64,
                ));
            }
        }
        out
    }

    fn prefix_free(ls: &[String]) -> bool {
        ls.iter()
            .all(|a| ls.iter().filter(|b| b.starts_with(a.as_str())).count() == 1)
    }

    #[test]
    fn labels_are_unique_and_prefix_free() {
        for (cols, rows) in [(1, 1), (3, 2), (6, 4), (7, 4), (12, 9), (20, 20)] {
            let ls = labels(&lattice(cols, rows));
            assert_eq!(ls.len(), cols * rows);
            assert!(ls.iter().all(|l| !l.is_empty()));
            let set: HashSet<&String> = ls.iter().collect();
            assert_eq!(set.len(), ls.len(), "{cols}x{rows} labels repeat");
            assert!(prefix_free(&ls), "{cols}x{rows} labels are not prefix-free");
        }
    }

    #[test]
    fn one_key_per_element_up_to_twenty_six() {
        let ls = labels(&lattice(13, 2));
        assert!(ls.iter().all(|l| l.len() == 1));
        // Past 26 only the crowded keys grow a second character.
        let ls = labels(&lattice(13, 3));
        assert!(ls.iter().any(|l| l.len() == 2));
        assert!(ls.iter().all(|l| l.len() <= 2));
    }

    #[test]
    fn corners_land_on_the_matching_keys() {
        let ls = labels(&lattice(10, 3));
        assert_eq!(ls.first().unwrap(), "q");
        assert_eq!(ls[9], "p");
        assert!(ls.last().unwrap().starts_with('m'));
    }

    #[test]
    fn a_single_row_of_elements_uses_the_home_row() {
        let ls = labels(&lattice(5, 1));
        assert_eq!(ls, ["a", "d", "g", "j", "l"]);
    }

    #[test]
    fn a_single_column_of_elements_walks_down_the_keyboard() {
        let ls = labels(&lattice(1, 3));
        assert_eq!(ls, ["y", "g", "v"]);
    }

    #[test]
    fn neighbours_keep_their_reading_order() {
        let ls = labels(&lattice(9, 3));
        let keys = keys();
        // Elements in one row take keys from left to right on the keyboard,
        // and each row of elements sits on its own row of keys.
        for row in 0..3 {
            let mut prev = -1.0;
            for c in 0..9 {
                let first = ls[row * 9 + c].chars().next().unwrap();
                let key = keys.iter().find(|k| k.ch == first).unwrap();
                assert!(
                    ROWS[row].contains(first),
                    "row {row} column {c} left its key row"
                );
                assert!(key.x >= prev, "row {row} column {c} moved backwards");
                prev = key.x;
            }
        }
    }

    #[test]
    fn no_elements_no_labels() {
        assert!(labels(&[]).is_empty());
    }
}
