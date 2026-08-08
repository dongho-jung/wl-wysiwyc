use crate::grid::ROWS;

/// Every key of the qwerty block in reading order: q to p, a to l, z to m.
fn keys() -> Vec<char> {
    ROWS.iter().flat_map(|r| r.chars()).collect()
}

/// The key whose cell covers a position in the window, both axes normalized.
/// Rows split the height evenly and each row splits the width among its own
/// letters, so the block sits over the window exactly like the grid does.
fn key_at(x: f64, y: f64) -> usize {
    let r = ((y.clamp(0.0, 1.0) * ROWS.len() as f64) as usize).min(ROWS.len() - 1);
    let cols = ROWS[r].chars().count();
    let c = ((x.clamp(0.0, 1.0) * cols as f64) as usize).min(cols - 1);
    ROWS[..r].iter().map(|s| s.chars().count()).sum::<usize>() + c
}

/// Order a group the way it reads: top to bottom, then left to right.
fn reading_order(centers: &[(f64, f64)], group: &[usize]) -> Vec<usize> {
    let mut out = group.to_vec();
    out.sort_by(|&a, &b| {
        centers[a]
            .1
            .total_cmp(&centers[b].1)
            .then(centers[a].0.total_cmp(&centers[b].0))
    });
    out
}

/// Tell the elements sharing a key apart with a further key each, handed out
/// along the group in reading order. The first key already says where the
/// group is, so the rest of the label uses the keyboard end to end instead of
/// squeezing a column of elements into the three keys above each other.
fn suffixes(centers: &[(f64, f64)], group: &[usize], prefix: &str, out: &mut [String]) {
    let keys = keys();
    let order = reading_order(centers, group);
    if order.len() <= keys.len() {
        for (n, &i) in order.iter().enumerate() {
            out[i] = format!("{prefix}{}", keys[n]);
        }
        return;
    }
    // More elements than keys: each key takes a run of them and becomes a
    // prefix in turn.
    for (n, part) in order.chunks(order.len().div_ceil(keys.len())).enumerate() {
        let prefix = format!("{prefix}{}", keys[n]);
        match part {
            [i] => out[*i] = prefix,
            _ => suffixes(centers, part, &prefix, out),
        }
    }
}

/// Labels for elements at the given window-relative centers, returned in the
/// same order, for a window of `w` by `h`.
///
/// The first key is where the element is. The qwerty block is laid over the
/// window and an element takes the key covering it, never a neighbour's: an
/// element in the bottom-left corner is labelled Z, one in the top-right P,
/// however the elements happen to be spread. A key covering one element is
/// the whole label; a key covering several becomes their prefix. Labels are
/// prefix-free, so a complete label is never the start of another one.
pub fn labels(centers: &[(f64, f64)], w: f64, h: f64) -> Vec<String> {
    let mut out = vec![String::new(); centers.len()];
    let (w, h) = (w.max(1.0), h.max(1.0));
    let keys = keys();
    let mut cells: Vec<Vec<usize>> = vec![Vec::new(); keys.len()];
    for (i, &(x, y)) in centers.iter().enumerate() {
        cells[key_at(x / w, y / h)].push(i);
    }
    for (k, cell) in cells.iter().enumerate() {
        let prefix = keys[k].to_string();
        match cell.as_slice() {
            [] => {}
            [i] => out[*i] = prefix,
            _ => suffixes(centers, cell, &prefix, &mut out),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const W: f64 = 1000.0;
    const H: f64 = 1000.0;

    /// Centers of a cols x rows arrangement over the whole window, in
    /// reading order.
    fn lattice(cols: usize, rows: usize) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        for r in 0..rows {
            for c in 0..cols {
                out.push((
                    (c as f64 + 0.5) * W / cols as f64,
                    (r as f64 + 0.5) * H / rows as f64,
                ));
            }
        }
        out
    }

    fn first(label: &str) -> char {
        label.chars().next().unwrap()
    }

    fn prefix_free(ls: &[String]) -> bool {
        ls.iter()
            .all(|a| ls.iter().filter(|b| b.starts_with(a.as_str())).count() == 1)
    }

    #[test]
    fn labels_are_unique_and_prefix_free() {
        for (cols, rows) in [(1, 1), (3, 2), (6, 4), (7, 4), (12, 9), (20, 20)] {
            let ls = labels(&lattice(cols, rows), W, H);
            assert_eq!(ls.len(), cols * rows);
            assert!(ls.iter().all(|l| !l.is_empty()));
            let set: HashSet<&String> = ls.iter().collect();
            assert_eq!(set.len(), ls.len(), "{cols}x{rows} labels repeat");
            assert!(prefix_free(&ls), "{cols}x{rows} labels are not prefix-free");
        }
    }

    #[test]
    fn the_first_key_is_the_one_covering_the_element() {
        let centers = lattice(11, 7);
        let ls = labels(&centers, W, H);
        let keys = keys();
        for (&(x, y), label) in centers.iter().zip(&ls) {
            assert_eq!(
                first(label),
                keys[key_at(x / W, y / H)],
                "element at ({x}, {y}) borrowed another key"
            );
        }
    }

    #[test]
    fn a_crowded_side_stays_on_that_side_of_the_keyboard() {
        // A sidebar: forty elements down the left edge, nothing elsewhere.
        let centers: Vec<(f64, f64)> = (0..40)
            .map(|i| (20.0, (i as f64 + 0.5) * H / 40.0))
            .collect();
        let ls = labels(&centers, W, H);
        assert!(
            ls.iter().all(|l| "qaz".contains(first(l))),
            "left edge reached keys it does not sit under: {ls:?}"
        );
        // And they still read top to bottom down the keyboard.
        assert_eq!(first(&ls[0]), 'q');
        assert_eq!(first(ls.last().unwrap()), 'z');
    }

    #[test]
    fn corners_land_on_the_matching_keys() {
        let ls = labels(&lattice(10, 3), W, H);
        assert_eq!(ls[0], "q");
        assert_eq!(ls[9], "p");
        assert_eq!(first(ls.last().unwrap()), 'm');
    }

    #[test]
    fn a_single_row_of_elements_uses_the_home_row() {
        let ls = labels(&lattice(5, 1), W, H);
        assert_eq!(ls, ["a", "d", "g", "j", "l"]);
    }

    #[test]
    fn a_single_column_of_elements_walks_down_the_keyboard() {
        let ls = labels(&lattice(1, 3), W, H);
        assert_eq!(ls, ["y", "g", "v"]);
    }

    #[test]
    fn one_key_per_element_where_the_window_is_sparse() {
        let ls = labels(&lattice(7, 3), W, H);
        assert!(ls.iter().all(|l| l.len() == 1));
    }

    #[test]
    fn a_crowded_cell_grows_a_second_key_in_reading_order() {
        // Five elements inside one cell, top-left of the window.
        let centers: Vec<(f64, f64)> = (0..5).map(|i| (10.0, 10.0 + i as f64 * 20.0)).collect();
        let ls = labels(&centers, W, H);
        assert_eq!(ls, ["qq", "qw", "qe", "qr", "qt"]);
    }

    #[test]
    fn no_elements_no_labels() {
        assert!(labels(&[], W, H).is_empty());
    }
}
