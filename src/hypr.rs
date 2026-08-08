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

/// The submap the overlay's keys live in, named after the app so the
/// shortcut ids and the submap read the same in `hyprctl binds`.
const SUBMAP: &str = "wl-wysiwyc";

fn hyprctl(args: &[&str]) -> Result<String, Box<dyn Error>> {
    let out = Command::new("hyprctl").args(args).output()?;
    let reply = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() {
        return Err(format!("hyprctl {} failed: {reply}", args.join(" ")).into());
    }
    Ok(reply)
}

/// Whether the submap already holds our binds. Defining it twice appends a
/// second copy of every bind, and a key bound twice fires twice, which would
/// confirm a hint the moment it is armed.
fn submap_defined() -> bool {
    hyprctl(&["binds"]).is_ok_and(|b| b.contains(&format!("submap: {SUBMAP}")))
}

/// Define the submap that turns each key into a global shortcut. Hyprland
/// takes config either as Lua or in its own language depending on how it was
/// set up, and only one of the two answers to a given call, so try both.
fn define_submap(keys: &[String]) -> Result<(), Box<dyn Error>> {
    let lua_binds: String = keys
        .iter()
        .map(|k| format!("hl.bind(\"{k}\", hl.dsp.global(\"{SUBMAP}:{k}\")) "))
        .collect();
    let lua = format!("hl.define_submap(\"{SUBMAP}\", function() {lua_binds}end)");
    if hyprctl(&["eval", &lua]).is_ok_and(|r| r == "ok") {
        return Ok(());
    }
    let mut batch = format!("keyword submap {SUBMAP} ; ");
    for k in keys {
        batch.push_str(&format!("keyword bind ,{k},global,{SUBMAP}:{k} ; "));
    }
    batch.push_str("keyword submap reset");
    hyprctl(&["--batch", &batch])?;
    Ok(())
}

/// Send the compositor into the overlay's submap, defining it first if this
/// is the first run since Hyprland started.
pub fn enter_submap(keys: &[String]) -> Result<(), Box<dyn Error>> {
    if !submap_defined() {
        define_submap(keys)?;
        if !submap_defined() {
            return Err(format!("submap {SUBMAP} did not take").into());
        }
    }
    dispatch_submap(SUBMAP)?;
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
    if let Err(e) = Command::new("sh")
        .args(["-c", &script])
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
