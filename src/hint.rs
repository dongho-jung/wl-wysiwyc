use crate::config::Layout;
use crate::grid::rows_for;

/// Every key of the block in reading order: for qwerty, q to p, a to l, z to
/// m.
fn keys(rows: &[&str; 3]) -> Vec<char> {
    rows.iter().flat_map(|r| r.chars()).collect()
}

/// The key whose cell covers a position in the window, both axes normalized.
/// Rows split the height evenly and each row splits the width among its own
/// letters, so the block sits over the window exactly like the grid does.
fn key_at(rows: &[&str; 3], x: f64, y: f64) -> usize {
    let r = ((y.clamp(0.0, 1.0) * rows.len() as f64) as usize).min(rows.len() - 1);
    let cols = rows[r].chars().count();
    let c = ((x.clamp(0.0, 1.0) * cols as f64) as usize).min(cols - 1);
    rows[..r].iter().map(|s| s.chars().count()).sum::<usize>() + c
}

/// Where a key sits on the block, both axes normalized, so keys can be
/// compared to each other and to element positions.
fn key_pos(rows: &[&str; 3], k: usize) -> (f64, f64) {
    let mut seen = 0;
    for (r, row) in rows.iter().enumerate() {
        let cols = row.chars().count();
        if k < seen + cols {
            return (
                (k - seen) as f64 / cols as f64 + 0.5 / cols as f64,
                (r as f64 + 0.5) / rows.len() as f64,
            );
        }
        seen += cols;
    }
    (0.5, 0.5)
}

/// Move the overflow of any crowded key to the nearest key with room.
///
/// The first key is meant to say where its element is, so nothing moves while
/// its own key can hold it. A strip of forty icons down one edge overruns the
/// two or three keys covering that edge though, and the choice there is
/// between a third key on every one of them and a short hop to a neighbour.
/// The hop wins: it keeps every label two keys long and only the overflow
/// moves, to the key nearest the element it is leaving.
fn relieve_crowding(
    rows: &[&str; 3],
    norm: &[(f64, f64)],
    cells: &mut [Vec<usize>],
    room: &[usize],
) {
    loop {
        let Some(from) = cells
            .iter()
            .enumerate()
            .position(|(k, c)| c.len() > room[k])
        else {
            return;
        };
        let (fx, fy) = key_pos(rows, from);
        let Some(to) = (0..cells.len())
            .filter(|&k| cells[k].len() < room[k])
            .min_by(|&a, &b| {
                let d = |k: usize| {
                    let (x, y) = key_pos(rows, k);
                    (x - fx).powi(2) + (y - fy).powi(2)
                };
                d(a).total_cmp(&d(b))
            })
        else {
            return; // Every key is full: the window has more targets than
                    // two keys can name, and suffixes() takes it from here.
        };
        // Send whichever element already sits closest to the key taking it.
        let (tx, ty) = key_pos(rows, to);
        let at = cells[from]
            .iter()
            .enumerate()
            .min_by(|(_, &a), (_, &b)| {
                let d = |i: usize| {
                    let (x, y) = norm[i];
                    (x - tx).powi(2) + (y - ty).powi(2)
                };
                d(a).total_cmp(&d(b))
            })
            .map(|(at, _)| at)
            .unwrap_or(0);
        let moved = cells[from].remove(at);
        cells[to].push(moved);
    }
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
fn suffixes(
    centers: &[(f64, f64)],
    group: &[usize],
    prefix: &str,
    out: &mut [String],
    keys: &[char],
) {
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
            _ => suffixes(centers, part, &prefix, out, keys),
        }
    }
}

/// Labels for elements at the given window-relative centers, returned in the
/// same order, for a window of `w` by `h`.
///
/// The first key is where the element is. The keyboard is laid over the
/// window and an element takes the key covering it, never a neighbour's: on
/// qwerty an element in the bottom-left corner is labelled Z, one in the
/// top-right P, however the elements happen to be spread. A key covering one
/// element is the whole label; a key covering several becomes their prefix.
/// Labels are prefix-free, so a complete label is never the start of another
/// one.
///
/// `Layout::None` gives that up, since there is no keyboard to follow: labels
/// then run a to z in reading order.
pub fn labels(
    centers: &[(f64, f64)],
    w: f64,
    h: f64,
    reserved: &[char],
    layout: Layout,
) -> Vec<String> {
    let mut out = vec![String::new(); centers.len()];
    let (w, h) = (w.max(1.0), h.max(1.0));
    let rows = rows_for(layout);
    let all = keys(rows);
    // A key that means something else, or one asked to be left out, cannot
    // name a hint. Its cell holds nothing, so whatever sits under it moves to
    // the keys around it.
    let free: Vec<char> = all
        .iter()
        .copied()
        .filter(|k| !reserved.contains(k))
        .collect();
    if free.is_empty() {
        return out;
    }
    if layout == Layout::None {
        // One group over the whole window, handed out in reading order.
        let every: Vec<usize> = (0..centers.len()).collect();
        if !every.is_empty() {
            suffixes(centers, &every, "", &mut out, &free);
        }
        return out;
    }
    // How many elements one key can hold and still tell them apart with a
    // single further key: one per key left to spell a suffix with. Past that
    // the label would need a third key, which is worse than moving the
    // overflow one key over.
    let per_key = free.len();
    let room: Vec<usize> = all
        .iter()
        .map(|k| if reserved.contains(k) { 0 } else { per_key })
        .collect();
    let norm: Vec<(f64, f64)> = centers
        .iter()
        .map(|&(x, y)| ((x / w).clamp(0.0, 1.0), (y / h).clamp(0.0, 1.0)))
        .collect();
    let mut cells: Vec<Vec<usize>> = vec![Vec::new(); all.len()];
    for (i, &(x, y)) in norm.iter().enumerate() {
        cells[key_at(rows, x, y)].push(i);
    }
    relieve_crowding(rows, &norm, &mut cells, &room);
    for (k, cell) in cells.iter().enumerate() {
        let prefix = all[k].to_string();
        match cell.as_slice() {
            [] => {}
            [i] => out[*i] = prefix,
            _ => suffixes(centers, cell, &prefix, &mut out, &free),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The default keyboard, which every test below but the layout ones uses.
    fn qwerty() -> &'static [&'static str; 3] {
        rows_for(Layout::Qwerty)
    }

    /// Labels on qwerty, the arrangement these tests describe.
    fn on_qwerty(centers: &[(f64, f64)], reserved: &[char]) -> Vec<String> {
        labels(centers, W, H, reserved, Layout::Qwerty)
    }

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
            let ls = on_qwerty(&lattice(cols, rows), &[]);
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
        let ls = on_qwerty(&centers, &[]);
        let keys = keys(qwerty());
        for (&(x, y), label) in centers.iter().zip(&ls) {
            assert_eq!(
                first(label),
                keys[key_at(qwerty(), x / W, y / H)],
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
        let ls = on_qwerty(&centers, &[]);
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
        let ls = on_qwerty(&lattice(10, 3), &[]);
        assert_eq!(ls[0], "q");
        assert_eq!(ls[9], "p");
        assert_eq!(first(ls.last().unwrap()), 'm');
    }

    #[test]
    fn a_single_row_of_elements_uses_the_home_row() {
        let ls = on_qwerty(&lattice(5, 1), &[]);
        assert_eq!(ls, ["a", "d", "g", "j", "l"]);
    }

    #[test]
    fn a_single_column_of_elements_walks_down_the_keyboard() {
        let ls = on_qwerty(&lattice(1, 3), &[]);
        assert_eq!(ls, ["y", "g", "v"]);
    }

    #[test]
    fn one_key_per_element_where_the_window_is_sparse() {
        let ls = on_qwerty(&lattice(7, 3), &[]);
        assert!(ls.iter().all(|l| l.len() == 1));
    }

    #[test]
    fn a_dense_strip_still_fits_in_two_keys() {
        // A sidebar of eighty icons down the left edge covers three keys,
        // which hold 26 each; the rest hop to the keys next to them rather
        // than pushing every label to three characters.
        let centers: Vec<(f64, f64)> = (0..80)
            .map(|i| (20.0, (i as f64 + 0.5) * H / 80.0))
            .collect();
        let ls = on_qwerty(&centers, &[]);
        assert!(
            ls.iter().all(|l| l.len() <= 2),
            "labels grew a third key: {ls:?}"
        );
        assert!(prefix_free(&ls));
        // What moves, moves to a neighbour: nothing reaches the far side.
        assert!(
            ls.iter().all(|l| !"opl".contains(first(l))),
            "the left edge reached the right of the keyboard: {ls:?}"
        );
    }

    #[test]
    fn a_crowded_cell_grows_a_second_key_in_reading_order() {
        // Five elements inside one cell, top-left of the window.
        let centers: Vec<(f64, f64)> = (0..5).map(|i| (10.0, 10.0 + i as f64 * 20.0)).collect();
        let ls = on_qwerty(&centers, &[]);
        assert_eq!(ls, ["qq", "qw", "qe", "qr", "qt"]);
    }

    #[test]
    fn a_reserved_key_names_nothing() {
        // 'a' clicks, so no label may start with it and the elements that sit
        // under it move to the keys beside it.
        let ls = on_qwerty(&lattice(9, 3), &['a']);
        assert!(
            ls.iter().all(|l| !l.contains('a')),
            "a reserved key turned up in a label: {ls:?}"
        );
        assert_eq!(ls.len(), 27);
        assert!(prefix_free(&ls));
    }

    #[test]
    fn no_elements_no_labels() {
        assert!(on_qwerty(&[], &[]).is_empty());
    }

    #[test]
    fn excluded_letters_never_turn_up() {
        let out: Vec<char> = "tyughvbn".chars().collect();
        let ls = on_qwerty(&lattice(9, 7), &out);
        assert_eq!(ls.len(), 63);
        assert!(
            ls.iter().all(|l| !l.chars().any(|c| out.contains(&c))),
            "an excluded key turned up in a label: {ls:?}"
        );
        assert!(ls.iter().all(|l| l.len() <= 2), "labels grew: {ls:?}");
        assert!(prefix_free(&ls));
    }

    #[test]
    fn dvorak_puts_its_own_letters_in_the_corners() {
        let ls = labels(&lattice(10, 3), W, H, &[], Layout::Dvorak);
        assert_eq!(ls[0], "p");
        assert_eq!(ls[9], "l");
        assert_eq!(first(&ls[20]), 'q');
        assert_eq!(first(ls.last().unwrap()), 'z');
    }

    #[test]
    fn without_a_layout_labels_run_a_to_z_in_reading_order() {
        let ls = labels(&lattice(4, 3), W, H, &[], Layout::None);
        assert_eq!(
            ls,
            ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"]
        );
        // And past the alphabet they pair up, still in reading order.
        let many = labels(&lattice(10, 4), W, H, &[], Layout::None);
        assert_eq!(many[0], "aa");
        assert!(prefix_free(&many));
        assert_eq!(many.iter().collect::<HashSet<_>>().len(), 40);
    }

    #[test]
    fn every_layout_keeps_its_labels_unique() {
        for layout in [Layout::Qwerty, Layout::Dvorak, Layout::None] {
            for (cols, rows) in [(1, 1), (6, 4), (12, 9)] {
                let ls = labels(&lattice(cols, rows), W, H, &['e'], layout);
                assert!(ls.iter().all(|l| !l.is_empty()), "{layout:?} left a blank");
                assert!(
                    ls.iter().all(|l| !l.contains('e')),
                    "{layout:?} used a reserved key"
                );
                assert_eq!(
                    ls.iter().collect::<HashSet<_>>().len(),
                    ls.len(),
                    "{layout:?} {cols}x{rows} labels repeat"
                );
                assert!(prefix_free(&ls), "{layout:?} {cols}x{rows} not prefix-free");
            }
        }
    }
}
