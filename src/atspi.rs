use std::collections::{HashSet, VecDeque};
use std::error::Error;
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

// AtspiStateType bit positions.
const STATE_SENSITIVE: u64 = 24;
const STATE_SHOWING: u64 = 25;

const MAX_NODES: usize = 4000;
const MAX_ELEMENTS: usize = 400;
const WALK_BUDGET: Duration = Duration::from_millis(1200);

/// A clickable element, window-relative logical coordinates.
#[derive(Debug, Clone)]
pub struct Element {
    pub role: u32,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
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
pub fn clickable_elements(pid: i32, title: &str) -> Result<Vec<Element>, Box<dyn Error>> {
    if pid <= 0 {
        return Err("window has no pid".into());
    }
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
    let conn = zbus::blocking::connection::Builder::address(addr.as_str())?.build()?;

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
    let frame = frames
        .iter()
        .find(|(d, p)| name_of(&conn, d, p.as_str()).as_deref() == Some(title))
        .unwrap_or(&frames[0])
        .clone();

    let frame_ext = extents_of(&conn, &frame.0, frame.1.as_str());
    let (frame_w, frame_h) = match frame_ext {
        Some((_, _, w, h)) if w > 0 && h > 0 => (w as f64, h as f64),
        _ => (f64::MAX, f64::MAX),
    };

    let deadline = Instant::now() + WALK_BUDGET;
    let mut queue: VecDeque<(String, String, f64, Option<String>)> = VecDeque::new();
    queue.push_back((frame.0.clone(), frame.1.to_string(), 1.0, None));
    let mut visited = 0usize;
    let mut seen_rects = HashSet::new();
    let mut out = Vec::new();

    while let Some((node_dest, path, ratio, parent_path)) = queue.pop_front() {
        if visited >= MAX_NODES || out.len() >= MAX_ELEMENTS || Instant::now() > deadline {
            break;
        }
        if path == NULL_PATH {
            continue;
        }
        visited += 1;

        let Some(role) = role_of(&conn, &node_dest, &path) else {
            continue;
        };
        let st = state_of(&conn, &node_dest, &path);
        let showing = st & (1 << STATE_SHOWING) != 0;
        if !showing && visited > 1 {
            continue;
        }

        if CLICKABLE_ROLES.contains(&role) && st & (1 << STATE_SENSITIVE) != 0 {
            if let Some((x, y, w, h)) = extents_of(&conn, &node_dest, &path) {
                let (ex, ey, ew, eh) = (
                    x as f64 / ratio,
                    y as f64 / ratio,
                    w as f64 / ratio,
                    h as f64 / ratio,
                );
                let on_screen =
                    ex + ew > 0.0 && ey + eh > 0.0 && ex < frame_w && ey < frame_h;
                if ew >= 3.0 && eh >= 3.0 && on_screen {
                    let key = (ex as i32, ey as i32, ew as i32, eh as i32, role);
                    if seen_rects.insert(key) {
                        out.push(Element {
                            role,
                            x: ex,
                            y: ey,
                            w: ew,
                            h: eh,
                        });
                    }
                }
            }
        }

        let mut child_ratio = ratio;
        if role == ROLE_DOCUMENT_WEB {
            if let (Some(parent), Some((_, _, dw, _))) =
                (parent_path.as_deref(), extents_of(&conn, &node_dest, &path))
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

        for (cd, cp) in children(&conn, &node_dest, &path) {
            queue.push_back((cd, cp.to_string(), child_ratio, Some(path.clone())));
        }
    }

    out.sort_by(|a, b| (a.y, a.x).partial_cmp(&(b.y, b.x)).unwrap());
    Ok(out)
}
