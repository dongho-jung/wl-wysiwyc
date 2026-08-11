use serde::Deserialize;
use std::error::Error;
use std::process::Command;

#[derive(Deserialize)]
struct MonitorJson {
    name: String,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    scale: f64,
    transform: i32,
    focused: bool,
    #[serde(rename = "activeWorkspace")]
    active_workspace: WorkspaceRef,
}

#[derive(Deserialize)]
struct WorkspaceRef {
    id: i64,
}

#[derive(Deserialize)]
struct ClientJson {
    mapped: bool,
    hidden: bool,
    // Older Hyprland versions do not report this field; assume visible.
    // Distinguishes the active window of a group from the tabs behind it.
    #[serde(default = "default_true")]
    visible: bool,
    at: [i32; 2],
    size: [i32; 2],
    workspace: WorkspaceRef,
    class: String,
    title: String,
    pid: i32,
    // 0 is the focused window. Older Hyprland versions do not report this
    // field; those clients sort last and the first window wins instead.
    #[serde(rename = "focusHistoryID", default = "default_focus_id")]
    focus_history_id: i64,
}

fn default_true() -> bool {
    true
}

fn default_focus_id() -> i64 {
    i64::MAX
}

#[derive(Debug, Clone)]
pub struct Monitor {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub logical_w: i32,
    pub logical_h: i32,
    pub scale: f64,
}

#[derive(Debug, Clone)]
pub struct Window {
    pub class: String,
    pub title: String,
    pub pid: i32,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub monitor: Monitor,
    pub windows: Vec<Window>,
    /// Index into `windows` of the focused window, where the overlay starts.
    pub focused: usize,
    /// Bottom-right corner of the whole output layout, logical pixels.
    /// Used as the extent for virtual-pointer absolute motion.
    pub layout_extent: (i32, i32),
}

/// The key that leaves the submap without asking this process anything.
const RESCUE: &str = "CTRL + escape";
/// The app id the global shortcuts are registered under.
const APP: &str = "wl-wysiwyc";

/// The submap the overlay's keys live in. The name carries a digest of the
/// keys themselves, because a submap cannot be cleared: defining one twice
/// appends a second copy of every bind, and a key bound twice would take a
/// hint the moment it is shown. Changing which keys the overlay wants
/// therefore means a new submap rather than a redefinition of this one.
fn submap_name(keys: &[(String, String, bool, bool)]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let joined: Vec<String> = keys
        .iter()
        .map(|(id, bind, repeat, transparent)| format!("{id}={bind}:{repeat}:{transparent}"))
        .collect();
    for byte in format!("modifier-navigation-v3 {}", joined.join(" ")).bytes() {
        h ^= byte as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("wl-wysiwyc.{:08x}", h as u32)
}

fn hyprctl(args: &[&str]) -> Result<String, Box<dyn Error>> {
    let out = Command::new("hyprctl").args(args).output()?;
    let reply = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() {
        return Err(format!("hyprctl {} failed: {reply}", args.join(" ")).into());
    }
    Ok(reply)
}

/// Whether this submap already holds our binds.
fn submap_defined(name: &str) -> bool {
    hyprctl(&["binds"]).is_ok_and(|b| b.contains(&format!("submap: {name}")))
}

fn lua_bind_options(repeat: bool, transparent: bool) -> &'static str {
    match (repeat, transparent) {
        (false, false) => "",
        (true, false) => ", { repeating = true }",
        (false, true) => ", { transparent = true }",
        (true, true) => ", { repeating = true, transparent = true }",
    }
}

fn legacy_bind_directive(repeat: bool, transparent: bool) -> &'static str {
    match (repeat, transparent) {
        (false, false) => "bind",
        (true, false) => "binde",
        (false, true) => "bindt",
        (true, true) => "bindet",
    }
}

/// Define the submap that turns each key into a global shortcut. Hyprland
/// takes config either as Lua or in its own language depending on how it was
/// set up, and only one of the two answers to a given call, so try both.
fn define_submap(name: &str, keys: &[(String, String, bool, bool)]) -> Result<(), Box<dyn Error>> {
    let mut lua_binds: String = keys
        .iter()
        .map(|(id, bind, repeat, transparent)| {
            let options = lua_bind_options(*repeat, *transparent);
            format!("hl.bind(\"{bind}\", hl.dsp.global(\"{APP}:{id}\"){options}) ")
        })
        .collect();
    // A way out that does not depend on this process being alive to answer.
    // Every other key in the submap dispatches to it, so if it wedges or is
    // killed between the submap going up and coming down, this is the only
    // key that still does anything.
    lua_binds.push_str(&format!("hl.bind(\"{RESCUE}\", hl.dsp.submap(\"reset\")) "));
    let lua = format!("hl.define_submap(\"{name}\", function() {lua_binds}end)");
    if hyprctl(&["eval", &lua]).is_ok_and(|r| r == "ok") {
        return Ok(());
    }
    let mut batch = format!("keyword submap {name} ; ");
    for (id, bind, repeat, transparent) in keys {
        // The legacy syntax wants the modifiers in one field of their own,
        // space separated, and the key in the next.
        let (mods, key) = match bind.rsplit_once('+') {
            Some((m, k)) => (m.replace('+', " ").trim().to_string(), k.trim().to_string()),
            None => (String::new(), bind.clone()),
        };
        let directive = legacy_bind_directive(*repeat, *transparent);
        batch.push_str(&format!(
            "keyword {directive} {mods},{key},global,{APP}:{id} ; "
        ));
    }
    batch.push_str("keyword bind CTRL,escape,submap,reset ; ");
    batch.push_str("keyword submap reset");
    hyprctl(&["--batch", &batch])?;
    Ok(())
}

/// Send the compositor into the overlay's submap, defining it first if this
/// is the first run since Hyprland started.
pub fn enter_submap(keys: &[(String, String, bool, bool)]) -> Result<(), Box<dyn Error>> {
    let name = submap_name(keys);
    if !submap_defined(&name) {
        define_submap(&name, keys)?;
        if !submap_defined(&name) {
            return Err(format!("submap {name} did not take").into());
        }
    }
    dispatch_submap(&name)?;
    watch_for_death();
    Ok(())
}

/// Leave the submap even if this process is killed rather than asked to
/// quit. Every key in the submap dispatches to this client, so dying inside
/// it would leave the keyboard aimed at a process that is gone. A killed
/// process runs no destructors, so the guard has to live outside it.
fn watch_for_death() {
    let pid = std::process::id();
    let script = format!(
        "while kill -0 {pid} 2>/dev/null; do sleep 0.2; done; \
         pgrep -x wl-wysiwyc >/dev/null && exit 0; \
         hyprctl dispatch 'hl.dsp.submap(\"reset\")' >/dev/null 2>&1 || \
         hyprctl dispatch submap reset >/dev/null 2>&1"
    );
    if let Err(e) = Command::new("setsid")
        .args(["-f", "sh", "-c", &script])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        eprintln!("submap watchdog: {e}");
    }
}

pub fn leave_submap() -> Result<(), Box<dyn Error>> {
    dispatch_submap("reset")
}

fn dispatch_submap(name: &str) -> Result<(), Box<dyn Error>> {
    let lua = format!("hl.dsp.submap(\"{name}\")");
    if hyprctl(&["dispatch", &lua]).is_ok_and(|r| r == "ok") {
        return Ok(());
    }
    match hyprctl(&["dispatch", "submap", name])?.as_str() {
        "ok" => Ok(()),
        other => Err(format!("hyprctl dispatch submap {name}: {other}").into()),
    }
}

/// Where the pointer is, in global logical coordinates. Wayland only tells a
/// client about the pointer when it is over one of its surfaces, and this
/// overlay deliberately takes no pointer input, so the compositor has to be
/// asked directly.
pub fn cursor_pos() -> Option<(f64, f64)> {
    #[derive(Deserialize)]
    struct Pos {
        x: f64,
        y: f64,
    }
    let raw = hyprctl_json("cursorpos").ok()?;
    let pos: Pos = serde_json::from_slice(&raw).ok()?;
    Some((pos.x, pos.y))
}

fn hyprctl_json(arg: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let out = Command::new("hyprctl").args(["-j", arg]).output()?;
    if !out.status.success() {
        return Err(format!(
            "hyprctl -j {arg} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(out.stdout)
}

fn logical_size(m: &MonitorJson) -> (i32, i32) {
    // Odd transforms are 90/270 degree rotations, which swap width and height.
    let (w, h) = if m.transform % 2 == 1 {
        (m.height, m.width)
    } else {
        (m.width, m.height)
    };
    (
        (w as f64 / m.scale).round() as i32,
        (h as f64 / m.scale).round() as i32,
    )
}

pub fn snapshot() -> Result<Snapshot, Box<dyn Error>> {
    let monitors: Vec<MonitorJson> = serde_json::from_slice(&hyprctl_json("monitors")?)?;
    let clients: Vec<ClientJson> = serde_json::from_slice(&hyprctl_json("clients")?)?;

    let mut extent = (0, 0);
    for m in &monitors {
        let (lw, lh) = logical_size(m);
        extent.0 = extent.0.max(m.x + lw);
        extent.1 = extent.1.max(m.y + lh);
    }

    let focused = monitors
        .iter()
        .find(|m| m.focused)
        .or_else(|| monitors.first())
        .ok_or("hyprctl reported no monitors")?;
    let (lw, lh) = logical_size(focused);
    let monitor = Monitor {
        name: focused.name.clone(),
        x: focused.x,
        y: focused.y,
        logical_w: lw,
        logical_h: lh,
        scale: focused.scale,
    };

    let ws = focused.active_workspace.id;
    let mut kept: Vec<ClientJson> = clients
        .into_iter()
        .filter(|c| {
            c.mapped
                && !c.hidden
                && c.visible
                && c.workspace.id == ws
                && c.size[0] > 0
                && c.size[1] > 0
        })
        .collect();
    kept.sort_by_key(|c| (c.at[1], c.at[0]));

    let mut focused_idx = kept
        .iter()
        .enumerate()
        .min_by_key(|(_, c)| c.focus_history_id)
        .map(|(i, _)| i)
        .unwrap_or(0);
    // Picker keys are 1-9, so anything past nine windows is unreachable there.
    // The focused window is where the overlay starts, so keep it either way.
    if focused_idx >= 9 {
        kept.swap(8, focused_idx);
        focused_idx = 8;
    }
    kept.truncate(9);

    let windows: Vec<Window> = kept
        .into_iter()
        .map(|c| Window {
            class: c.class,
            title: c.title,
            pid: c.pid,
            x: c.at[0],
            y: c.at[1],
            w: c.size[0],
            h: c.size[1],
        })
        .collect();

    Ok(Snapshot {
        focused: focused_idx.min(windows.len().saturating_sub(1)),
        monitor,
        windows,
        layout_extent: extent,
    })
}

#[cfg(test)]
mod tests {
    use super::{legacy_bind_directive, lua_bind_options};

    #[test]
    fn moving_arrows_repeat_and_cannot_be_shadowed() {
        assert_eq!(legacy_bind_directive(true, true), "bindet");
        assert_eq!(
            lua_bind_options(true, true),
            ", { repeating = true, transparent = true }"
        );
    }

    #[test]
    fn instant_arrows_are_transparent_but_do_not_repeat() {
        assert_eq!(legacy_bind_directive(false, true), "bindt");
        assert_eq!(lua_bind_options(false, true), ", { transparent = true }");
    }
}
