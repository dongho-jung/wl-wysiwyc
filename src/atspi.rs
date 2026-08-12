use std::cmp::Reverse;
use std::collections::{HashSet, VecDeque};
use std::error::Error;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const IFACE_ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const IFACE_COMPONENT: &str = "org.a11y.atspi.Component";
const NULL_PATH: &str = "/org/a11y/atspi/null";
const COORD_WINDOW: u32 = 1;

// AtspiRole values from at-spi2 atspi-constants.h.
const ROLE_DOCUMENT_WEB: u32 = 95;
pub const CLICKABLE_ROLES: &[u32] = &[
    7,  // check box
    8,  // check menu item
    11, // combo box
    32, // list item
    35, // menu item
    37, // page tab
    40, // password text
    43, // push button
    44, // radio button
    45, // radio menu item
    51, // slider
    52, // spin button
    62, // toggle button
    79, // entry
    88, // link
    91, // tree item
];

/// Roles that usually wrap the thing you actually want to click: a list row
/// around its link, a tree item around its button. They keep their own hint
/// only when they hold nothing clickable.
const CONTAINER_ROLES: &[u32] = &[
    32, // list item
    91, // tree item
];

// AtspiStateType bit positions.
const STATE_SENSITIVE: u64 = 24;
const STATE_SHOWING: u64 = 25;

const MAX_NODES: usize = 4000;

/// How many nodes the walk reads at once. Wide enough to keep the bus busy,
/// narrow enough that the priority order below still means something.
const BATCH: usize = 32;

fn actionable_state(state: u64) -> bool {
    state & (1 << STATE_SHOWING) != 0 && state & (1 << STATE_SENSITIVE) != 0
}

/// A node waiting to be walked: where it lives, the pixel ratio inherited
/// from its document, the parent that ratio was measured against, and
/// whether it is inside a document, which is the part of a window that
/// scrolls.
type Pending = (String, String, f64, Option<String>, bool);

/// A clickable element, window-relative logical coordinates. The node it came
/// from is kept so its box can be looked at again later, which is how a
/// window that scrolls under the overlay is noticed.
#[derive(Debug, Clone)]
pub struct Element {
    pub role: u32,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub dest: String,
    pub path: String,
    /// Inside a document, so it moves when the window is scrolled. The
    /// chrome around one does not.
    pub scrolls: bool,
    /// The pixel ratio its extents were reported in, so the same node can be
    /// measured again later and compared.
    pub ratio: f64,
}

pub fn role_name(role: u32) -> &'static str {
    match role {
        7 => "check box",
        8 => "check menu item",
        11 => "combo box",
        32 => "list item",
        35 => "menu item",
        37 => "page tab",
        40 => "password text",
        43 => "button",
        44 => "radio button",
        45 => "radio menu item",
        51 => "slider",
        52 => "spin button",
        62 => "toggle button",
        79 => "entry",
        88 => "link",
        91 => "tree item",
        _ => "?",
    }
}

/// Whether `inner` sits within `outer`, allowing for the rounding an
/// accessibility tree does on its own extents.
fn inside(inner: &Element, outer: &Element) -> bool {
    const T: f64 = 2.0;
    inner.x >= outer.x - T
        && inner.y >= outer.y - T
        && inner.x + inner.w <= outer.x + outer.w + T
        && inner.y + inner.h <= outer.y + outer.h + T
}

/// Whether two elements cover the same spot closely enough that two hints
/// would land on top of each other.
fn same_spot(a: &Element, b: &Element) -> bool {
    const T: f64 = 4.0;
    (a.x - b.x).abs() <= T
        && (a.y - b.y).abs() <= T
        && (a.w - b.w).abs() <= T
        && (a.h - b.h).abs() <= T
}

/// One hint per target. Trees routinely nest a link inside a list row inside
/// a cell, all with near-identical extents, and hinting every level of that
/// buries the window in labels. Wrappers give way to what they wrap, and
/// what is left keeps one element per spot.
fn prune(els: Vec<Element>) -> Vec<Element> {
    let is_container = |e: &Element| CONTAINER_ROLES.contains(&e.role);
    let area = |e: &Element| e.w * e.h;
    let mut kept: Vec<Element> = Vec::with_capacity(els.len());
    for (i, e) in els.iter().enumerate() {
        let wraps_a_target = is_container(e)
            && els
                .iter()
                .enumerate()
                .any(|(j, o)| j != i && inside(o, e) && (!is_container(o) || area(o) < area(e)));
        if wraps_a_target || kept.iter().any(|k| same_spot(k, e)) {
            continue;
        }
        kept.push(e.clone());
    }
    kept
}

/// Everything the walk wants to know about one node.
#[derive(Default)]
struct NodeInfo {
    role: Option<u32>,
    state: u64,
    extents: Option<(i32, i32, i32, i32)>,
    kids: Vec<(String, OwnedObjectPath)>,
}

/// Read a batch of nodes with their calls overlapping.
///
/// Four round trips per node, made one after another, is what the walk used
/// to spend nearly all of its time on: the bus is idle while each reply
/// travels. A zbus connection matches replies to calls by serial, so calls
/// from several threads can be outstanding on the one connection at once, and
/// a batch then costs roughly what the application takes to answer it rather
/// than a round trip per call.
fn read_batch(conn: &Connection, batch: &[Pending]) -> Vec<NodeInfo> {
    if batch.len() == 1 {
        let (dest, path, ..) = &batch[0];
        return vec![read_node(conn, dest, path)];
    }
    std::thread::scope(|scope| {
        let threads: Vec<_> = batch
            .iter()
            .map(|(dest, path, ..)| {
                let conn = conn.clone();
                scope.spawn(move || read_node(&conn, dest, path))
            })
            .collect();
        threads
            .into_iter()
            .map(|t| t.join().unwrap_or_default())
            .collect()
    })
}

fn read_node(conn: &Connection, dest: &str, path: &str) -> NodeInfo {
    NodeInfo {
        role: role_of(conn, dest, path),
        state: state_of(conn, dest, path),
        extents: extents_of(conn, dest, path),
        kids: children(conn, dest, path),
    }
}

fn children(conn: &Connection, dest: &str, path: &str) -> Vec<(String, OwnedObjectPath)> {
    let Ok(m) = conn.call_method(Some(dest), path, Some(IFACE_ACCESSIBLE), "GetChildren", &())
    else {
        return Vec::new();
    };
    m.body().deserialize().unwrap_or_default()
}

fn role_of(conn: &Connection, dest: &str, path: &str) -> Option<u32> {
    let m = conn
        .call_method(Some(dest), path, Some(IFACE_ACCESSIBLE), "GetRole", &())
        .ok()?;
    m.body().deserialize().ok()
}

fn state_of(conn: &Connection, dest: &str, path: &str) -> u64 {
    let Ok(m) = conn.call_method(Some(dest), path, Some(IFACE_ACCESSIBLE), "GetState", &()) else {
        return 0;
    };
    let words: Vec<u32> = m.body().deserialize().unwrap_or_default();
    let lo = words.first().copied().unwrap_or(0) as u64;
    let hi = words.get(1).copied().unwrap_or(0) as u64;
    lo | (hi << 32)
}

fn extents_of(conn: &Connection, dest: &str, path: &str) -> Option<(i32, i32, i32, i32)> {
    let m = conn
        .call_method(
            Some(dest),
            path,
            Some(IFACE_COMPONENT),
            "GetExtents",
            &COORD_WINDOW,
        )
        .ok()?;
    m.body().deserialize().ok()
}

fn name_of(conn: &Connection, dest: &str, path: &str) -> Option<String> {
    let m = conn
        .call_method(
            Some(dest),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(IFACE_ACCESSIBLE, "Name"),
        )
        .ok()?;
    let v: OwnedValue = m.body().deserialize().ok()?;
    String::try_from(v).ok()
}

/// Find the window's application on the accessibility bus by pid and collect
/// the clickable elements of the frame whose name matches the window title.
/// Chromium reports web-content extents in physical pixels while the UI parts
/// use logical pixels; the ratio is derived at each document-web node from
/// its own extents versus its parent's and divided back out.
fn bus() -> Result<Connection, Box<dyn Error>> {
    let session = Connection::session()?;
    let addr: String = session
        .call_method(
            Some("org.a11y.Bus"),
            "/org/a11y/bus",
            Some("org.a11y.Bus"),
            "GetAddress",
            &(),
        )?
        .body()
        .deserialize()?;
    Ok(zbus::blocking::connection::Builder::address(addr.as_str())?.build()?)
}

/// Watch one node's box and report how far it moves.
///
/// A window that scrolls under the overlay leaves every hint naming where
/// something used to be, and nothing tells the overlay that happened: it
/// takes no pointer input, so the wheel that did it went straight past.
/// Asking one node where it is, twenty times a second, is one round trip and
/// catches it, and the answer is also how far everything inside the document
/// moved, which is what the labels need to follow along. The receiver going
/// away is what stops the thread.
pub fn watch_bounds(el: &Element) -> Option<Receiver<(f64, f64)>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let conn = bus().ok()?;
    let (dest, path, ratio) = (el.dest.clone(), el.path.clone(), el.ratio);
    let mut last = extents_of(&conn, &dest, &path);
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(45));
        let now = extents_of(&conn, &dest, &path);
        if now == last {
            continue;
        }
        let moved = match (last, now) {
            (Some(a), Some(b)) => ((b.0 - a.0) as f64 / ratio, (b.1 - a.1) as f64 / ratio),
            // The node came or went, so nothing sensible to move by; the
            // read that follows sorts it out.
            _ => (0.0, 0.0),
        };
        last = now;
        if tx.send(moved).is_err() {
            return;
        }
    });
    Some(rx)
}

pub fn clickable_elements(
    pid: i32,
    title: &str,
    expected_size: (i32, i32),
) -> Result<Vec<Element>, Box<dyn Error>> {
    if pid <= 0 {
        return Err("window has no pid".into());
    }
    let conn = bus()?;

    let apps: Vec<(String, OwnedObjectPath)> = conn
        .call_method(
            Some("org.a11y.atspi.Registry"),
            "/org/a11y/atspi/accessible/root",
            Some(IFACE_ACCESSIBLE),
            "GetChildren",
            &(),
        )?
        .body()
        .deserialize()?;

    let mut dest = None;
    for (bus_name, _) in &apps {
        let m = conn.call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetConnectionUnixProcessID",
            bus_name,
        );
        let app_pid: u32 = match m.and_then(|m| m.body().deserialize()) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if app_pid == pid as u32 {
            dest = Some(bus_name.clone());
            break;
        }
    }
    let dest = dest.ok_or("window's process is not on the accessibility bus")?;

    let frames = children(&conn, &dest, "/org/a11y/atspi/accessible/root");
    if frames.is_empty() {
        return Err("application exposes no windows on the accessibility bus".into());
    }
    let named_frames: Vec<_> = frames
        .iter()
        .map(|(dest, path)| {
            (
                dest.clone(),
                path.clone(),
                name_of(&conn, dest, path.as_str()).unwrap_or_default(),
                extents_of(&conn, dest, path.as_str()),
            )
        })
        .collect();
    let exact = named_frames
        .iter()
        .filter(|(_, _, name, _)| name == title)
        .count();
    let wanted_words = title_words(title);
    let score = |(_, _, name, extents): &(_, _, String, Option<(i32, i32, i32, i32)>)| {
        frame_score(name, *extents, &wanted_words, expected_size)
    };
    let candidates = || named_frames.iter().enumerate();
    let selected = if exact > 0 {
        candidates()
            .filter(|(_, (_, _, name, _))| name == title)
            .min_by_key(|(_, frame)| score(frame))
            .map(|(index, _)| index)
            .unwrap_or(0)
    } else {
        candidates()
            .min_by_key(|(_, frame)| score(frame))
            .map(|(index, _)| index)
            .unwrap_or(0)
    };
    let frame = &named_frames[selected];
    let frame = (frame.0.clone(), frame.1.clone());
    if std::env::var_os("WL_TRACE").is_some() {
        let names = named_frames
            .iter()
            .map(|(_, _, name, extents)| format!("{name:?}@{extents:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        eprintln!(
            "NAV atspi pid={pid} requested={title:?} size={expected_size:?} \
             exact={} selected={selected} frames=[{names}]",
            exact,
        );
    }

    let frame_ext = extents_of(&conn, &frame.0, frame.1.as_str());
    let (frame_w, frame_h) = match frame_ext {
        Some((_, _, w, h)) if w > 0 && h > 0 => (w as f64, h as f64),
        _ => (f64::MAX, f64::MAX),
    };

    let cfg = &crate::config::get().elements;
    let deadline = Instant::now() + Duration::from_millis(cfg.walk_ms);
    // Nodes to walk: the ones inside the window first, everything else after.
    let mut queue: VecDeque<Pending> = VecDeque::new();
    let mut later: VecDeque<Pending> = VecDeque::new();
    queue.push_back((frame.0.clone(), frame.1.to_string(), 1.0, None, false));
    let mut visited = 0usize;
    let mut seen_rects = HashSet::new();
    let mut out = Vec::new();

    let on_screen = |(ex, ey, ew, eh): (f64, f64, f64, f64)| {
        ex + ew > 0.0 && ey + eh > 0.0 && ex < frame_w && ey < frame_h
    };

    'walk: loop {
        // Take a batch, whichever queue has something, and read all of it at
        // once. One node is four round trips and a window is a few hundred
        // nodes; waiting for each reply in turn is nearly all of the pause
        // before the overlay appears.
        let mut batch: Vec<Pending> = Vec::with_capacity(BATCH);
        while batch.len() < BATCH {
            match queue.pop_front().or_else(|| later.pop_front()) {
                Some(node) if node.1 == NULL_PATH => continue,
                Some(node) => batch.push(node),
                None => break,
            }
        }
        if batch.is_empty() {
            break;
        }
        visited += batch.len();
        let read = read_batch(&conn, &batch);

        for ((node_dest, path, ratio, parent_path, in_doc), info) in batch.into_iter().zip(read) {
            if out.len() >= cfg.max {
                break 'walk;
            }
            let Some(role) = info.role else {
                continue;
            };
            let st = info.state;
            let showing = st & (1 << STATE_SHOWING) != 0;

            // A node's own rectangle, once Chromium's pixel ratio is divided
            // out.
            let rect = info.extents.map(|(x, y, w, h)| {
                (
                    x as f64 / ratio,
                    y as f64 / ratio,
                    w as f64 / ratio,
                    h as f64 / ratio,
                )
            });

            // Whether this node's own box is worth walking into first. A
            // container that sits off the window usually has nothing visible
            // inside it, and a long page has far more of those than visible
            // ones. Chromium is the exception that stops this being a prune:
            // it gives a scroll container the box of its whole contents, which
            // for a scrolled page is nowhere near the window, while the
            // visible rows inside it are placed correctly. So off-window
            // subtrees go to the back of the walk instead of being dropped,
            // and a page that has hidden a widget in a corner still gets
            // hinted once the visible part is done.
            let promising =
                showing && rect.is_none_or(|r| r.2 <= 0.0 || r.3 <= 0.0 || on_screen(r));

            if CLICKABLE_ROLES.contains(&role) && actionable_state(st) {
                if let Some((ex, ey, ew, eh)) = rect {
                    if ew >= 3.0 && eh >= 3.0 && on_screen((ex, ey, ew, eh)) {
                        let key = (ex as i32, ey as i32, ew as i32, eh as i32, role);
                        if seen_rects.insert(key) {
                            out.push(Element {
                                role,
                                x: ex,
                                y: ey,
                                w: ew,
                                h: eh,
                                dest: node_dest.clone(),
                                path: path.clone(),
                                scrolls: in_doc,
                                ratio,
                            });
                        }
                    }
                }
            }

            let mut child_ratio = ratio;
            if role == ROLE_DOCUMENT_WEB {
                // The ratio has to come from the raw extents, since dividing
                // by the current ratio is the very thing being corrected.
                if let (Some(parent), Some((_, _, dw, _))) = (parent_path.as_deref(), info.extents)
                {
                    if let Some((_, _, pw, _)) = extents_of(&conn, &node_dest, parent) {
                        if pw > 50 && dw > 0 {
                            let r = dw as f64 / pw as f64;
                            if (0.75..=4.0).contains(&r) {
                                child_ratio = ratio * r;
                            }
                        }
                    }
                }
            }

            let next = if promising { &mut queue } else { &mut later };
            for (cd, cp) in info.kids {
                next.push_back((
                    cd,
                    cp.to_string(),
                    child_ratio,
                    Some(path.clone()),
                    in_doc || role == ROLE_DOCUMENT_WEB,
                ));
            }
        }

        if visited >= MAX_NODES || Instant::now() > deadline {
            // Running out of budget drops whatever the walk had not reached:
            // the deepest nodes, and the off-window ones held back above. A
            // heavy page keeps its chrome and loses part of its content, so
            // say so instead of silently hinting a subset.
            if !queue.is_empty() || !later.is_empty() {
                eprintln!(
                    "atspi: walk stopped early after {visited} nodes, {} elements",
                    out.len()
                );
            }
            break;
        }
    }

    out.sort_by(|a, b| (a.y, a.x).partial_cmp(&(b.y, b.x)).unwrap());
    let out = prune(out);
    if std::env::var_os("WL_TRACE").is_some() {
        let page = out.iter().filter(|element| element.scrolls).count();
        eprintln!(
            "NAV atspi-result elements={} page={page} frame=({frame_w:.0},{frame_h:.0})",
            out.len()
        );
    }
    Ok(out)
}

fn title_words(title: &str) -> HashSet<String> {
    title
        .split(|character: char| !character.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| word.len() >= 3 && !matches!(word.as_str(), "chromium" | "html" | "nav"))
        .collect()
}

fn frame_score(
    name: &str,
    extents: Option<(i32, i32, i32, i32)>,
    wanted_words: &HashSet<String>,
    expected_size: (i32, i32),
) -> (i64, Reverse<usize>) {
    let size_error = extents.map_or(i64::MAX, |(_, _, width, height)| {
        i64::from((width - expected_size.0).abs()) + i64::from((height - expected_size.1).abs())
    });
    let words = title_words(name);
    let shared = words.intersection(wanted_words).count();
    (size_error, Reverse(shared))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(role: u32, x: f64, y: f64, w: f64, h: f64) -> Element {
        Element {
            role,
            x,
            y,
            w,
            h,
            dest: String::new(),
            path: String::new(),
            scrolls: false,
            ratio: 1.0,
        }
    }

    const LIST_ITEM: u32 = 32;
    const LINK: u32 = 88;
    const BUTTON: u32 = 43;

    #[test]
    fn chromium_frame_selection_uses_geometry_then_title_words() {
        let wanted = title_words("nav: grid - Chromium");
        let expected = (1588, 1076);
        let generator = frame_score(
            "Generator - Chromium",
            Some((0, 0, 1588, 326)),
            &wanted,
            expected,
        );
        let wrong_page = frame_score(
            "form.html - Chromium",
            Some((0, 0, 1588, 1076)),
            &wanted,
            expected,
        );
        let grid = frame_score(
            "grid.html - Chromium",
            Some((0, 0, 1588, 1076)),
            &wanted,
            expected,
        );
        let duplicate_title = frame_score(
            "nav: grid - Chromium",
            Some((0, 0, 1588, 326)),
            &wanted,
            expected,
        );
        let exact_title = frame_score(
            "nav: grid - Chromium",
            Some((0, 0, 1588, 1076)),
            &wanted,
            expected,
        );
        assert!(grid < generator, "a different-sized window ranked first");
        assert!(grid < wrong_page, "title words did not break a size tie");
        assert!(
            exact_title < duplicate_title,
            "duplicate titles did not use window geometry"
        );
    }

    #[test]
    fn a_row_gives_way_to_the_link_inside_it() {
        let els = vec![
            el(LIST_ITEM, 0.0, 0.0, 400.0, 60.0),
            el(LINK, 8.0, 4.0, 120.0, 20.0),
        ];
        let kept = prune(els);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].role, LINK);
    }

    #[test]
    fn a_row_with_nothing_clickable_keeps_its_hint() {
        let els = vec![
            el(LIST_ITEM, 0.0, 0.0, 400.0, 60.0),
            el(LINK, 8.0, 400.0, 120.0, 20.0),
        ];
        assert_eq!(prune(els).len(), 2);
    }

    #[test]
    fn stacked_duplicates_collapse_to_one() {
        let els = vec![
            el(BUTTON, 10.0, 10.0, 40.0, 40.0),
            el(LINK, 12.0, 11.0, 41.0, 39.0),
            el(BUTTON, 80.0, 10.0, 40.0, 40.0),
        ];
        let kept = prune(els);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].role, BUTTON);
        assert_eq!(kept[1].x, 80.0);
    }

    #[test]
    fn nested_rows_keep_the_innermost() {
        let els = vec![
            el(LIST_ITEM, 0.0, 0.0, 400.0, 200.0),
            el(LIST_ITEM, 10.0, 10.0, 380.0, 40.0),
        ];
        let kept = prune(els);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].h, 40.0);
    }

    #[test]
    fn showing_and_sensitive_are_enough_to_keep_a_target() {
        let state = (1 << STATE_SHOWING) | (1 << STATE_SENSITIVE);
        assert!(actionable_state(state));
        assert!(!actionable_state(1 << STATE_SHOWING));
        assert!(!actionable_state(1 << STATE_SENSITIVE));
    }

    #[test]
    fn a_small_button_inside_a_link_is_kept() {
        let card = el(LINK, 100.0, 100.0, 160.0, 160.0);
        let action = el(BUTTON, 220.0, 106.0, 32.0, 32.0);

        assert_eq!(prune(vec![card, action]).len(), 2);
    }
}
