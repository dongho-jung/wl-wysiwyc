mod atspi;
mod config;
mod draw;
mod grid;
mod hint;
mod hypr;
mod overlay;
mod shortcuts;

use std::error::Error;
use std::process::Command;
use std::time::Duration;

const USAGE: &str = "\
wl-wysiwyc: keyboard-driven window clicking for Hyprland

Usage:
  wl-wysiwyc                 interactive: hints the focused window right away,
                             or cancels an overlay that is already up.
                             Type a hint to send the pointer to it, then a
                             click key (- or Enter) to click. Keep the click
                             key down and move to drag, then release to drop.
                             Space switches to the letter grid, Tab picks
                             another window, Esc backs out.
                             Shift jumps to the last anchor that way, Alt
                             moves freely, and Ctrl jumps between anchors
  wl-wysiwyc --list          print the windows that would be shown
  wl-wysiwyc --elements [N]  print the clickable elements detected for
                             window N, or the focused window (debugging aid)
  wl-wysiwyc --smoke MS [N]  render the hint overlay for MS milliseconds
                             without grabbing the keyboard (debugging aid)
  wl-wysiwyc --smoke-grid MS [N]
                             like --smoke but shows the letter grid
  wl-wysiwyc --smoke-pick MS like --smoke but shows the window picker
  wl-wysiwyc --render FILE [N [KEYS]]
                             write what the overlay would draw to FILE as a
                             PAM, alpha and all, without showing anything.
                             KEYS is a run of presses: all but the last
                             confirmed, the last one armed, or all of them
                             with a trailing dot (debugging aid)
  wl-wysiwyc --drill SCRIPT [N]
                             put the overlay through a run of key presses on
                             the focused window and say where the pointer
                             went: down:70 wait:400 down+right:250 taps down,
                             waits, then holds a diagonal chord. Nothing is
                             clicked and no keys are taken (debugging aid)
  wl-wysiwyc --keys          print the keys the overlay would bind and what
                             each one does, without showing anything
  wl-wysiwyc --reset         leave the key submap, for when a run was killed
                             before it could (Ctrl+Esc does the same)
  wl-wysiwyc --move-test X Y move the cursor to global (X, Y) through the
                             virtual pointer, no click (debugging aid)
  wl-wysiwyc --help          show this help
";

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = args.first().map(String::as_str);
    let opt_window = |i: usize| match args.get(i) {
        Some(n) => n.parse::<usize>().map(Some),
        None => Ok(None),
    };
    let smoke = |i: usize, view: overlay::SmokeView| -> Result<overlay::Smoke, Box<dyn Error>> {
        let ms: u64 = args
            .get(i)
            .ok_or_else(|| format!("{} needs a duration in milliseconds", flag.unwrap_or("")))?
            .parse()?;
        Ok(overlay::Smoke {
            duration: Duration::from_millis(ms),
            view,
        })
    };
    match flag {
        None => interactive(None),
        Some("--list") => list(),
        Some("--elements") => elements(opt_window(1)?),
        Some("--smoke") => {
            let view = overlay::SmokeView::Hints(opt_window(2)?);
            interactive(Some(smoke(1, view)?))
        }
        Some("--smoke-grid") => {
            let view = overlay::SmokeView::Grid(opt_window(2)?);
            interactive(Some(smoke(1, view)?))
        }
        Some("--smoke-pick") => interactive(Some(smoke(1, overlay::SmokeView::Picker)?)),
        Some("--render") => {
            let path = args.get(1).ok_or("--render needs a file to write")?;
            render(path, opt_window(2)?, args.get(3).map_or("", String::as_str))
        }
        Some("--drill") => {
            let script = args.get(1).ok_or("--drill needs a run of key presses")?;
            drill(script, opt_window(2)?)
        }
        Some("--keys") => keys(),
        Some("--reset") => {
            hypr::leave_submap()?;
            println!("left the wl-wysiwyc submap");
            Ok(())
        }
        Some("--move-test") => {
            let x: f64 = args.get(1).ok_or("--move-test needs X and Y")?.parse()?;
            let y: f64 = args.get(2).ok_or("--move-test needs X and Y")?.parse()?;
            let snap = hypr::snapshot()?;
            overlay::move_only(&snap, (x, y))
        }
        Some("--help" | "-h") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown argument {other}; see --help").into()),
    }
}

/// What the submap would bind, and to what. Every key the overlay answers
/// for, so a config that gives one key two jobs can be seen without opening
/// the overlay to find out.
fn keys() -> Result<(), Box<dyn Error>> {
    for key in shortcuts::keys() {
        let bindings = key.bindings().join(", ");
        println!("{:<10} {bindings}", key.name());
    }
    Ok(())
}

fn list() -> Result<(), Box<dyn Error>> {
    let snap = hypr::snapshot()?;
    let m = &snap.monitor;
    println!(
        "monitor {} at ({}, {}) logical {}x{} scale {}",
        m.name, m.x, m.y, m.logical_w, m.logical_h, m.scale
    );
    println!(
        "layout extent {}x{}",
        snap.layout_extent.0, snap.layout_extent.1
    );
    for (i, w) in snap.windows.iter().enumerate() {
        println!(
            "{}:{} at ({}, {}) size {}x{} [{}] {}",
            i + 1,
            if i == snap.focused { " focused" } else { "" },
            w.x,
            w.y,
            w.w,
            w.h,
            w.class,
            w.title
        );
    }
    Ok(())
}

fn elements(n: Option<usize>) -> Result<(), Box<dyn Error>> {
    let snap = hypr::snapshot()?;
    let idx = match n {
        Some(n) => n.checked_sub(1).ok_or("window numbers start at 1")?,
        None => snap.focused,
    };
    let w = snap.windows.get(idx).ok_or("no such window; see --list")?;
    println!("window {}: [{}] {}", idx + 1, w.class, w.title);
    let els = atspi::clickable_elements(w.pid, &w.title, (w.w, w.h))?;
    let centers: Vec<(f64, f64)> = els
        .iter()
        .map(|e| (e.x + e.w / 2.0, e.y + e.h / 2.0))
        .collect();
    let reserved = config::get().keys.reserved_letters();
    let layout = config::get().keys.layout;
    let labels = hint::labels(&centers, w.w as f64, w.h as f64, &reserved, layout);
    println!(
        "{} clickable elements (window-relative logical):",
        els.len()
    );
    for (e, label) in els.iter().zip(&labels) {
        println!(
            "  {:4} {:16} at ({:.0}, {:.0}) size {:.0}x{:.0}",
            label,
            atspi::role_name(e.role),
            e.x,
            e.y,
            e.w,
            e.h
        );
    }
    Ok(())
}

/// Write what the overlay would draw to a binary PAM, alpha and all, with
/// nothing shown on screen. Laid over a screenshot of the desktop it is
/// exactly what the overlay would look like, so the look of a hint or an
/// armed key can be checked while the desktop is in use.
fn render(path: &str, n: Option<usize>, keys: &str) -> Result<(), Box<dyn Error>> {
    let snap = hypr::snapshot()?;
    let idx = match n {
        Some(n) => n.checked_sub(1).ok_or("window numbers start at 1")?,
        None => snap.focused,
    };
    let win = snap.windows.get(idx).ok_or("no such window; see --list")?;
    println!("window {}: [{}] {}", idx + 1, win.class, win.title);
    let (buf, w, h) = overlay::render(&snap, idx, keys)?;
    let mut out =
        format!("P7\nWIDTH {w}\nHEIGHT {h}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n")
            .into_bytes();
    // The overlay is premultiplied Argb8888 in memory order B, G, R, A;
    // image files want it straight.
    for px in buf.chunks_exact(4) {
        let a = px[3] as f32 / 255.0;
        for i in [2usize, 1, 0] {
            let v = if a > 0.0 { px[i] as f32 / a } else { 0.0 };
            out.push(v.round().clamp(0.0, 255.0) as u8);
        }
        out.push(px[3]);
    }
    std::fs::write(path, out)?;
    println!("wrote {path}: {w}x{h}");
    Ok(())
}

/// Another overlay is already up, so this launch means cancel it. The
/// keybind that starts the tool is the natural way to change your mind, and
/// pressing it twice should not leave two overlays fighting over the
/// keyboard.
fn cancel_running() -> bool {
    let me = std::process::id();
    let out = match Command::new("pgrep")
        .args(["-x", env!("CARGO_PKG_NAME")])
        .output()
    {
        Ok(o) => o,
        Err(_) => return false,
    };
    let others: Vec<u32> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .filter(|&pid: &u32| pid != me)
        .collect();
    if others.is_empty() {
        return false;
    }
    for pid in others {
        // SAFETY: a signal to a pid read back from pgrep; a stale pid is an
        // error the kernel reports rather than a signal to something else,
        // since pids are not reused while the process is still being waited
        // on by its parent.
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }
    // The overlay being cancelled cannot put the keyboard back itself, since
    // a signal runs no destructors.
    if let Err(e) = hypr::leave_submap() {
        eprintln!("cancel: {e}");
    }
    true
}

fn interactive(smoke: Option<overlay::Smoke>) -> Result<(), Box<dyn Error>> {
    run_overlay(smoke, None)
}

/// Put the overlay through a run of key presses and say where the pointer
/// went. Nothing is clicked and no keys are taken; this is for measuring how
/// navigating feels against a real window's labels.
fn drill(script: &str, win: Option<usize>) -> Result<(), Box<dyn Error>> {
    let mut steps = overlay::Drill::parse(script)?;
    steps.win = win;
    run_overlay(None, Some(steps))
}

fn run_overlay(
    smoke: Option<overlay::Smoke>,
    drill: Option<overlay::Drill>,
) -> Result<(), Box<dyn Error>> {
    if smoke.is_none() && drill.is_none() && cancel_running() {
        return Ok(());
    }
    let snap = hypr::snapshot()?;
    if snap.windows.is_empty() {
        return Err("no windows on the active workspace".into());
    }
    if let Some((x, y)) = overlay::run(snap, smoke, drill)? {
        println!("clicked at ({x:.0}, {y:.0})");
    }
    Ok(())
}
