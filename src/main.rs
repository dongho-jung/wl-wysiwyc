mod atspi;
mod draw;
mod grid;
mod hint;
mod hypr;
mod overlay;

use std::error::Error;
use std::time::Duration;

const USAGE: &str = "\
wl-wysiwyc: keyboard-driven window clicking for Hyprland

Usage:
  wl-wysiwyc                 interactive: hints the focused window right away.
                             Type a hint to arm it, press the same key again
                             to click. Space switches to the qwerty letter
                             grid, Tab picks another window, Esc backs out
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
                             PPM, without showing anything. KEYS is a run of
                             presses: all but the last confirmed, the last
                             one armed, or all of them with a trailing dot
                             (debugging aid)
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
    let els = atspi::clickable_elements(w.pid, &w.title)?;
    let centers: Vec<(f64, f64)> = els
        .iter()
        .map(|e| (e.x + e.w / 2.0, e.y + e.h / 2.0))
        .collect();
    let labels = hint::labels(&centers, w.w as f64, w.h as f64);
    println!("{} clickable elements (window-relative logical):", els.len());
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

/// Write what the overlay would draw to a binary PPM, over a flat grey
/// stand-in for the window under it. Nothing is shown on screen, so the
/// look of a hint or an armed key can be checked while the desktop is in
/// use.
fn render(path: &str, n: Option<usize>, keys: &str) -> Result<(), Box<dyn Error>> {
    const BACKDROP: [u8; 3] = [0xd6, 0xd6, 0xd8];
    let snap = hypr::snapshot()?;
    let idx = match n {
        Some(n) => n.checked_sub(1).ok_or("window numbers start at 1")?,
        None => snap.focused,
    };
    let win = snap.windows.get(idx).ok_or("no such window; see --list")?;
    println!("window {}: [{}] {}", idx + 1, win.class, win.title);
    let (buf, w, h) = overlay::render(&snap, idx, keys)?;
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    // The overlay is premultiplied Argb8888 in memory order B, G, R, A.
    for px in buf.chunks_exact(4) {
        let inv = 1.0 - px[3] as f32 / 255.0;
        for (i, back) in [2usize, 1, 0].into_iter().zip(BACKDROP) {
            out.push((px[i] as f32 + back as f32 * inv).round().min(255.0) as u8);
        }
    }
    std::fs::write(path, out)?;
    println!("wrote {path}: {w}x{h}");
    Ok(())
}

fn interactive(smoke: Option<overlay::Smoke>) -> Result<(), Box<dyn Error>> {
    let snap = hypr::snapshot()?;
    if snap.windows.is_empty() {
        return Err("no windows on the active workspace".into());
    }
    if let Some((x, y)) = overlay::run(snap, smoke)? {
        println!("clicked at ({x:.0}, {y:.0})");
    }
    Ok(())
}
