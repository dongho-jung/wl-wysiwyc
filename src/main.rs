mod draw;
mod grid;
mod hypr;
mod overlay;

use std::error::Error;
use std::time::Duration;

const USAGE: &str = "\
wl-wysiwyc: keyboard-driven window clicking for Hyprland

Usage:
  wl-wysiwyc                 interactive: pick a window by number (1-9),
                             then a spot by letter (a-z), Esc goes back
  wl-wysiwyc --list          print the windows that would be shown
  wl-wysiwyc --smoke MS [N]  render the overlay for MS milliseconds without
                             grabbing the keyboard; with N, show the letter
                             grid for window N instead (debugging aid)
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
    match args.first().map(String::as_str) {
        None => interactive(None),
        Some("--list") => list(),
        Some("--smoke") => {
            let ms: u64 = args
                .get(1)
                .ok_or("--smoke needs a duration in milliseconds")?
                .parse()?;
            let stage_win = match args.get(2) {
                Some(n) => Some(n.parse::<usize>()?),
                None => None,
            };
            interactive(Some(overlay::Smoke {
                duration: Duration::from_millis(ms),
                grid_window: stage_win,
            }))
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
            "{}: at ({}, {}) size {}x{} [{}] {}",
            i + 1,
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
