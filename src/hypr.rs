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
}

fn default_true() -> bool {
    true
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
    /// Bottom-right corner of the whole output layout, logical pixels.
    /// Used as the extent for virtual-pointer absolute motion.
    pub layout_extent: (i32, i32),
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
    let mut windows: Vec<Window> = clients
        .into_iter()
        .filter(|c| {
            c.mapped
                && !c.hidden
                && c.visible
                && c.workspace.id == ws
                && c.size[0] > 0
                && c.size[1] > 0
        })
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
    windows.sort_by_key(|w| (w.y, w.x));
    // Selection keys are 1-9, so anything past nine windows is unreachable.
    windows.truncate(9);

    Ok(Snapshot {
        monitor,
        windows,
        layout_extent: extent,
    })
}
