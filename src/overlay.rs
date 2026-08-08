use crate::atspi::{self, Element};
use crate::draw::{self, Canvas, Color, Rect};
use crate::grid;
use crate::hint;
use crate::hypr::Snapshot;
use fontdue::Font;
use std::collections::HashMap;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_registry,
    delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use std::error::Error;
use std::time::{Duration, Instant};
use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_surface},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

const BTN_LEFT: u32 = 0x110;

const DIM: Color = Color::new(0.0, 0.0, 0.0, 0.28);
const SHADOW: Color = Color::new(0.0, 0.0, 0.0, 0.42);
const BOX_BG: Color = Color::new(0.09, 0.09, 0.12, 0.86);
const BOX_BORDER: Color = Color::new(1.0, 1.0, 1.0, 0.55);
const TEXT: Color = Color::new(1.0, 1.0, 1.0, 0.96);
const TILE_BG: Color = Color::new(0.08, 0.08, 0.10, 0.20);
const TILE_BORDER: Color = Color::new(1.0, 1.0, 1.0, 0.30);
const HINT_BG: Color = Color::new(0.98, 0.79, 0.29, 0.97);
const HINT_EDGE: Color = Color::new(0.30, 0.20, 0.01, 0.35);
const HINT_TEXT: Color = Color::new(0.14, 0.09, 0.0, 1.0);
const HINT_RING: Color = Color::new(1.0, 0.85, 0.35, 0.5);
/// The armed target, one key press away from being confirmed.
const ARMED_BG: Color = Color::new(0.24, 0.85, 0.60, 0.98);
const ARMED_EDGE: Color = Color::new(0.02, 0.26, 0.16, 0.55);
const ARMED_TEXT: Color = Color::new(0.0, 0.14, 0.08, 1.0);
const ARMED_RING: Color = Color::new(0.25, 0.92, 0.63, 0.95);
const ARMED_GLOW: Color = Color::new(0.25, 0.92, 0.63, 0.5);
const ARMED_FILL: Color = Color::new(0.24, 0.85, 0.60, 0.14);
/// The armed key itself, shown pressed inside the labels it keeps.
const ARMED_CAP: Color = Color::new(0.02, 0.24, 0.15, 0.92);
const ARMED_CAP_TEXT: Color = Color::new(0.55, 1.0, 0.82, 1.0);
const HUD_KEY: Color = Color::new(1.0, 1.0, 1.0, 0.14);

/// Label text size and the space around it, both in unscaled pixels.
const LABEL_PX: f32 = 13.0;
const LABEL_PAD_X: f32 = 5.0;
const LABEL_PAD_Y: f32 = 4.0;
/// Clearance between two labels, so a crowded window reads as separate
/// labels rather than one block of colour.
const LABEL_GAP: i32 = 3;

/// What a smoke run should render. The window index is 1-based; None means
/// the focused window, the same one the overlay starts on.
pub enum SmokeView {
    Hints(Option<usize>),
    Grid(Option<usize>),
    Picker,
}

pub struct Smoke {
    pub duration: Duration,
    pub view: SmokeView,
}

#[derive(Clone, Copy)]
#[allow(clippy::enum_variant_names)]
enum Stage {
    PickWindow,
    PickTile { win: usize },
    PickHint { win: usize },
}

#[derive(Clone)]
struct Hint {
    label: String,
    /// Element rectangle and click center, global logical coordinates.
    rx: f64,
    ry: f64,
    rw: f64,
    rh: f64,
    cx: f64,
    cy: f64,
}

struct App {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    pool: SlotPool,
    layer: Option<LayerSurface>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    snap: Snapshot,
    font: Font,
    buffer_scale: i32,
    width: u32,
    height: u32,
    configured: bool,
    dirty: bool,
    stage: Stage,
    exit: bool,
    target: Option<(f64, f64)>,
    hints: Vec<Hint>,
    /// Keys already confirmed, narrowing the hints.
    typed: String,
    /// A key pressed once and not confirmed yet: it previews what it would
    /// select, and pressing it again commits it.
    armed: Option<char>,
    elements_cache: HashMap<usize, Vec<Element>>,
    pending_pick: Option<usize>,
}

/// Show the overlay and return the chosen global click position, or None
/// if the user cancelled. The click itself is performed here as well,
/// after the overlay is torn down.
pub fn run(snap: Snapshot, smoke: Option<Smoke>) -> Result<Option<(f64, f64)>, Box<dyn Error>> {
    let font = draw::load_font()?;
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<App>(&conn)?;
    let qh = queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)?;
    let layer_shell = LayerShell::bind(&globals, &qh)?;
    let shm = Shm::bind(&globals, &qh)?;
    let pool = SlotPool::new(4096, &shm)?;
    let vp_manager: Option<ZwlrVirtualPointerManagerV1> = globals.bind(&qh, 1..=2, ()).ok();

    // A 1-based window number from the command line, the focused window when
    // there is none, and the focused window again when the number is out of
    // range.
    let window_or_focused = |n: Option<usize>| match n {
        Some(n) if n >= 1 && n <= snap.windows.len() => n - 1,
        _ => snap.focused,
    };
    let (stage, pending_pick) = match smoke.as_ref().map(|s| &s.view) {
        Some(SmokeView::Picker) => (Stage::PickWindow, None),
        Some(SmokeView::Grid(n)) => (
            Stage::PickTile {
                win: window_or_focused(*n),
            },
            None,
        ),
        Some(SmokeView::Hints(n)) => (Stage::PickWindow, Some(window_or_focused(*n))),
        None => (Stage::PickWindow, Some(snap.focused)),
    };
    let buffer_scale = (snap.monitor.scale.ceil() as i32).max(1);

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        shm,
        pool,
        layer: None,
        keyboard: None,
        snap,
        font,
        buffer_scale,
        width: 0,
        height: 0,
        configured: false,
        dirty: false,
        stage,
        exit: false,
        target: None,
        hints: Vec::new(),
        typed: String::new(),
        armed: None,
        elements_cache: HashMap::new(),
        pending_pick,
    };

    // Output names arrive with the initial burst of globals metadata.
    queue.roundtrip(&mut app)?;
    let output = app.find_output();

    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Overlay,
        Some("wl-wysiwyc"),
        output.as_ref(),
    );
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_size(0, 0);
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(if smoke.is_some() {
        KeyboardInteractivity::None
    } else {
        KeyboardInteractivity::Exclusive
    });
    layer.commit();
    app.layer = Some(layer);

    if let Some(smoke) = &smoke {
        let start = Instant::now();
        while start.elapsed() < smoke.duration {
            queue.roundtrip(&mut app)?;
            app.process_pending_pick();
            if app.configured && app.dirty {
                app.draw();
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    } else {
        while !app.exit {
            queue.blocking_dispatch(&mut app)?;
            app.process_pending_pick();
            if app.configured && app.dirty {
                app.draw();
            }
        }
    }

    // Unmap the overlay before injecting the click so the click reaches
    // the window below instead of this surface.
    app.layer.take();
    queue.roundtrip(&mut app)?;

    if let Some(target) = app.target {
        let mgr = vp_manager
            .as_ref()
            .ok_or("compositor does not expose zwlr_virtual_pointer_manager_v1")?;
        let seat = app.seat_state.seats().next();
        let vp = mgr.create_virtual_pointer(seat.as_ref(), &qh, ());
        let extent = app.snap.layout_extent;
        move_and_click(&vp, target, extent, true, &mut queue, &mut app)?;
        vp.destroy();
        queue.roundtrip(&mut app)?;
    }
    Ok(app.target)
}

/// Debug helper: move the cursor through the virtual pointer, no click.
pub fn move_only(snap: &Snapshot, target: (f64, f64)) -> Result<(), Box<dyn Error>> {
    struct Mini;
    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Mini {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    wayland_client::delegate_noop!(Mini: ignore ZwlrVirtualPointerManagerV1);
    wayland_client::delegate_noop!(Mini: ignore ZwlrVirtualPointerV1);

    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<Mini>(&conn)?;
    let qh = queue.handle();
    let mgr: ZwlrVirtualPointerManagerV1 = globals
        .bind(&qh, 1..=2, ())
        .map_err(|_| "compositor does not expose zwlr_virtual_pointer_manager_v1")?;
    let vp = mgr.create_virtual_pointer(None, &qh, ());
    let mut mini = Mini;
    move_and_click(&vp, target, snap.layout_extent, false, &mut queue, &mut mini)?;
    vp.destroy();
    queue.roundtrip(&mut mini)?;
    Ok(())
}

fn move_and_click<S>(
    vp: &ZwlrVirtualPointerV1,
    (gx, gy): (f64, f64),
    (ext_w, ext_h): (i32, i32),
    click: bool,
    queue: &mut wayland_client::EventQueue<S>,
    state: &mut S,
) -> Result<(), Box<dyn Error>> {
    let x = gx.round().clamp(0.0, (ext_w - 1) as f64) as u32;
    let y = gy.round().clamp(0.0, (ext_h - 1) as f64) as u32;
    vp.motion_absolute(0, x, y, ext_w as u32, ext_h as u32);
    vp.frame();
    queue.roundtrip(state)?;
    if click {
        std::thread::sleep(Duration::from_millis(20));
        vp.button(0, BTN_LEFT, wl_pointer::ButtonState::Pressed);
        vp.frame();
        vp.button(0, BTN_LEFT, wl_pointer::ButtonState::Released);
        vp.frame();
        queue.roundtrip(state)?;
    }
    Ok(())
}

impl App {
    fn find_output(&self) -> Option<wl_output::WlOutput> {
        for output in self.output_state.outputs() {
            if let Some(info) = self.output_state.info(&output) {
                if info.name.as_deref() == Some(self.snap.monitor.name.as_str()) {
                    return Some(output);
                }
            }
        }
        None
    }

    fn handle_key(&mut self, event: KeyEvent) {
        match event.keysym {
            Keysym::Escape => return self.cancel(),
            Keysym::BackSpace => return self.undo(),
            Keysym::Return | Keysym::KP_Enter => return self.commit_key(),
            Keysym::Tab => return self.pick_window(),
            _ => {}
        }
        let Some(ch) = event.utf8.as_deref().and_then(|s| s.chars().next()) else {
            return;
        };
        let ch = ch.to_ascii_lowercase();
        match self.stage {
            Stage::PickWindow => {
                if let Some(d) = ch.to_digit(10) {
                    let idx = d as usize;
                    if (1..=self.snap.windows.len()).contains(&idx) {
                        self.pending_pick = Some(idx - 1);
                    }
                }
            }
            Stage::PickTile { win } => {
                if ch == ' ' {
                    if !self.elements_cache.get(&win).is_none_or(Vec::is_empty) {
                        self.enter_hint_stage(win);
                    }
                    return;
                }
                self.press_tile(win, ch);
            }
            Stage::PickHint { win } => {
                if ch == ' ' {
                    self.stage = Stage::PickTile { win };
                    self.reset_input();
                    return;
                }
                self.press_hint(ch);
            }
        }
    }

    /// Esc unwinds one step at a time: drop the armed key, then everything
    /// confirmed so far, then quit.
    fn cancel(&mut self) {
        if self.armed.take().is_some() {
            self.dirty = true;
            return;
        }
        if !self.typed.is_empty() {
            self.typed.clear();
            self.dirty = true;
            return;
        }
        self.exit = true;
    }

    /// Backspace undoes one key press: the armed key if there is one, the
    /// last confirmed one otherwise.
    fn undo(&mut self) {
        if self.armed.take().is_some() || self.typed.pop().is_some() {
            self.dirty = true;
        }
    }

    fn reset_input(&mut self) {
        self.typed.clear();
        self.armed = None;
        self.dirty = true;
    }

    fn pick_window(&mut self) {
        if matches!(self.stage, Stage::PickWindow) {
            return;
        }
        self.stage = Stage::PickWindow;
        self.reset_input();
    }

    fn click(&mut self, target: (f64, f64)) {
        self.target = Some(target);
        self.exit = true;
    }

    /// The hints still reachable, that is the ones starting with what has
    /// been confirmed so far plus the given key.
    fn leads_anywhere(&self, ch: char) -> bool {
        let mut probe = self.typed.clone();
        probe.push(ch);
        self.hints.iter().any(|h| h.label.starts_with(&probe))
    }

    /// Confirm the armed key. Enter does this; so does pressing that key a
    /// second time.
    fn commit_key(&mut self) {
        let Some(ch) = self.armed.take() else {
            return;
        };
        self.dirty = true;
        match self.stage {
            Stage::PickTile { win } => {
                let w = &self.snap.windows[win];
                if let Some(t) = grid::tile_for(w.w as f64, w.h as f64, ch) {
                    self.click((w.x as f64 + t.x + t.w / 2.0, w.y as f64 + t.y + t.h / 2.0));
                }
            }
            Stage::PickHint { .. } => {
                self.typed.push(ch);
                if let Some(h) = self.hints.iter().find(|h| h.label == self.typed) {
                    self.click((h.cx, h.cy));
                }
            }
            Stage::PickWindow => {}
        }
    }

    fn press_tile(&mut self, win: usize, ch: char) {
        if !ch.is_ascii_lowercase() {
            return;
        }
        if self.armed == Some(ch) {
            self.commit_key();
            return;
        }
        let w = &self.snap.windows[win];
        if grid::tile_for(w.w as f64, w.h as f64, ch).is_some() {
            self.armed = Some(ch);
            self.dirty = true;
        }
    }

    /// Every key of a hint is confirmed on its own: the first press arms the
    /// key and shows what it would leave, the second press commits it, and
    /// committing the last key of a label clicks that element. A key that
    /// leads nowhere is ignored, and any other key moves the preview.
    fn press_hint(&mut self, ch: char) {
        if !ch.is_ascii_lowercase() {
            return;
        }
        if self.armed == Some(ch) {
            self.commit_key();
            return;
        }
        if self.leads_anywhere(ch) {
            self.armed = Some(ch);
            self.dirty = true;
        }
    }

    /// A window was picked: query its clickable elements (with a hard timeout
    /// so a stuck application cannot wedge the overlay) and enter hint mode,
    /// or fall back to the letter grid when nothing is found.
    fn process_pending_pick(&mut self) {
        let Some(win) = self.pending_pick.take() else {
            return;
        };
        if !self.elements_cache.contains_key(&win) {
            let w = &self.snap.windows[win];
            let (pid, title) = (w.pid, w.title.clone());
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(atspi::clickable_elements(pid, &title).map_err(|e| e.to_string()));
            });
            let els = match rx.recv_timeout(Duration::from_millis(1800)) {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    eprintln!("atspi: {e}");
                    Vec::new()
                }
                Err(_) => {
                    eprintln!("atspi: element query timed out");
                    Vec::new()
                }
            };
            self.elements_cache.insert(win, els);
        }
        if self.elements_cache.get(&win).is_none_or(Vec::is_empty) {
            self.stage = Stage::PickTile { win };
            self.reset_input();
        } else {
            self.enter_hint_stage(win);
        }
    }

    fn enter_hint_stage(&mut self, win: usize) {
        let els = self.elements_cache.get(&win).cloned().unwrap_or_default();
        self.hints = hints_for(&self.snap.windows[win], &els);
        self.stage = Stage::PickHint { win };
        self.reset_input();
    }

    fn draw(&mut self) {
        self.dirty = false;
        let Some(layer) = self.layer.as_ref() else {
            return;
        };
        let scale = self.buffer_scale;
        let bw = self.width as i32 * scale;
        let bh = self.height as i32 * scale;
        if bw <= 0 || bh <= 0 {
            return;
        }
        let (buffer, buf) = match self.pool.create_buffer(bw, bh, bw * 4, wl_shm::Format::Argb8888)
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!("buffer allocation failed: {e}");
                return;
            }
        };
        let mut canvas = Canvas { buf, w: bw, h: bh };
        canvas.clear(DIM);
        match self.stage {
            Stage::PickWindow => draw_pick_window(&self.snap, &self.font, &mut canvas, scale),
            Stage::PickTile { win } => {
                draw_pick_tile(&self.snap, win, self.armed, &self.font, &mut canvas, scale)
            }
            Stage::PickHint { win } => {
                let view = HintView {
                    hints: &self.hints,
                    typed: &self.typed,
                    armed: self.armed,
                };
                draw_pick_hint(&self.snap, win, view, &self.font, &mut canvas, scale)
            }
        }
        let surface = layer.wl_surface();
        surface.set_buffer_scale(scale);
        if buffer.attach_to(surface).is_err() {
            eprintln!("failed to attach buffer");
            return;
        }
        surface.damage_buffer(0, 0, bw, bh);
        surface.commit();
    }
}

/// Label a window's elements and place them in global coordinates.
fn hints_for(w: &crate::hypr::Window, els: &[Element]) -> Vec<Hint> {
    let centers: Vec<(f64, f64)> = els
        .iter()
        .map(|e| (e.x + e.w / 2.0, e.y + e.h / 2.0))
        .collect();
    els.iter()
        .zip(centers.iter())
        .zip(hint::labels(&centers, w.w as f64, w.h as f64))
        .map(|((e, &(cx, cy)), label)| Hint {
            label,
            rx: w.x as f64 + e.x,
            ry: w.y as f64 + e.y,
            rw: e.w,
            rh: e.h,
            cx: w.x as f64 + cx,
            cy: w.y as f64 + cy,
        })
        .collect()
}

/// Draw what the overlay would show into a plain buffer instead of onto the
/// screen, and return it with its size. `keys` is a run of key presses: all
/// but the last are confirmed, the last one is armed. Debugging aid, and the
/// only way to look at an armed overlay without holding the keyboard.
pub fn render(
    snap: &Snapshot,
    win: usize,
    keys: &str,
) -> Result<(Vec<u8>, i32, i32), Box<dyn Error>> {
    let font = draw::load_font()?;
    let scale = (snap.monitor.scale.ceil() as i32).max(1);
    let (bw, bh) = (
        snap.monitor.logical_w * scale,
        snap.monitor.logical_h * scale,
    );
    let mut buf = vec![0u8; (bw * bh * 4) as usize];
    let mut canvas = Canvas {
        buf: &mut buf,
        w: bw,
        h: bh,
    };
    canvas.clear(DIM);
    let w = snap.windows.get(win).ok_or("no such window; see --list")?;
    let els = atspi::clickable_elements(w.pid, &w.title).unwrap_or_else(|e| {
        eprintln!("atspi: {e}");
        Vec::new()
    });
    // Trailing "." means every key confirmed and none armed, the state right
    // after a confirming press.
    let mut typed = keys.to_string();
    let armed = match typed.strip_suffix('.') {
        Some(rest) => {
            typed = rest.to_string();
            None
        }
        None => typed.pop(),
    };
    if els.is_empty() {
        draw_pick_tile(snap, win, armed, &font, &mut canvas, scale);
    } else {
        let hints = hints_for(w, &els);
        let view = HintView {
            hints: &hints,
            typed: &typed,
            armed,
        };
        draw_pick_hint(snap, win, view, &font, &mut canvas, scale);
    }
    Ok((buf, bw, bh))
}

fn draw_pick_window(snap: &Snapshot, font: &Font, canvas: &mut Canvas, scale: i32) {
    let mon = &snap.monitor;
    let s = scale as f32;
    for (i, w) in snap.windows.iter().enumerate() {
        let frame = Rect::new(
            ((w.x - mon.x) * scale) as f32,
            ((w.y - mon.y) * scale) as f32,
            (w.w * scale) as f32,
            (w.h * scale) as f32,
        )
        .grow(-s);
        canvas.round_rect_outline(frame, 8.0 * s, 1.5 * s, TILE_BORDER);
        let side = (w.w.min(w.h) as f32 * 0.30).clamp(56.0, 140.0) * s;
        let card = Rect::new(
            frame.x + (frame.w - side) / 2.0,
            frame.y + (frame.h - side) / 2.0,
            side,
            side,
        );
        canvas.round_rect_shadow(card.shift(0.0, 2.0 * s), side * 0.24, 6.0 * s, SHADOW);
        canvas.round_rect(card, side * 0.24, BOX_BG);
        canvas.round_rect_outline(card, side * 0.24, 1.5 * s, BOX_BORDER);
        canvas.text_centered(
            font,
            &(i + 1).to_string(),
            card.x + side / 2.0,
            card.y + side / 2.0,
            side * 0.55,
            TEXT,
        );
    }
}

fn draw_pick_tile(
    snap: &Snapshot,
    win: usize,
    armed: Option<char>,
    font: &Font,
    canvas: &mut Canvas,
    scale: i32,
) {
    let mon = &snap.monitor;
    let w = &snap.windows[win];
    let rx = (w.x - mon.x) * scale;
    let ry = (w.y - mon.y) * scale;
    let rw = w.w * scale;
    let rh = w.h * scale;
    let s = scale as f32;
    for t in grid::tiles(rw as f64, rh as f64) {
        let hot = armed == Some(t.ch);
        let tile = Rect::new(
            rx as f32 + t.x as f32,
            ry as f32 + t.y as f32,
            t.w as f32,
            t.h as f32,
        )
        .grow(-1.5 * s);
        let radius = 10.0 * s;
        if hot {
            canvas.round_rect_shadow(tile, radius, 10.0 * s, ARMED_GLOW);
        }
        canvas.round_rect(tile, radius, if hot { ARMED_FILL } else { TILE_BG });
        canvas.round_rect_outline(
            tile,
            radius,
            if hot { 2.5 * s } else { s },
            if hot { ARMED_RING } else { TILE_BORDER },
        );
        let letter = t.ch.to_ascii_uppercase().to_string();
        let px = (t.w.min(t.h) as f32 * 0.42).min(64.0 * s);
        let (cx, cy) = (tile.x + tile.w / 2.0, tile.y + tile.h / 2.0);
        // The letter sits over whatever the window is showing, so it carries
        // its own shadow rather than trusting the background.
        canvas.text_centered(font, &letter, cx + s, cy + s, px, SHADOW);
        canvas.text_centered(font, &letter, cx, cy, px, TEXT);
    }
    let pending = Pending {
        typed: "",
        armed,
        clinches: true,
    };
    draw_hud(canvas, font, window_rect(snap, win, scale), pending, s);
}

/// Where a label sits on screen: buffer pixels, already clamped to the
/// canvas.
struct LabelBox {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl LabelBox {
    fn rect(&self) -> Rect {
        Rect::new(self.x as f32, self.y as f32, self.w as f32, self.h as f32)
    }

    /// Whether two labels are close enough to read as one blob.
    fn crowds(&self, o: &LabelBox, gap: i32) -> bool {
        self.x - gap < o.x + o.w
            && o.x - gap < self.x + self.w
            && self.y - gap < o.y + o.h
            && o.y - gap < self.y + self.h
    }
}

/// Lay out one box per hint. Labels want the top-left corner of their
/// element, vimium style, but a dense tree stacks them on top of each other,
/// so a box that would land on an earlier one, or right up against it, tries
/// the element's other corners and sides first. Placement covers every hint,
/// not just the visible ones, so labels stay put while a prefix is typed.
fn place_labels(
    hints: &[Hint],
    font: &Font,
    mon: (i32, i32),
    canvas: (i32, i32),
    scale: i32,
) -> Vec<LabelBox> {
    let px = LABEL_PX * scale as f32;
    let pad_x = (LABEL_PAD_X * scale as f32) as i32;
    let pad_y = (LABEL_PAD_Y * scale as f32) as i32;
    let gap = LABEL_GAP * scale;
    let mut taken: Vec<LabelBox> = Vec::with_capacity(hints.len());
    for h in hints {
        let ex = ((h.rx - mon.0 as f64) * scale as f64) as i32;
        let ey = ((h.ry - mon.1 as f64) * scale as f64) as i32;
        let ew = (h.rw * scale as f64) as i32;
        let eh = (h.rh * scale as f64) as i32;
        let bh = px as i32 + 2 * pad_y;
        // A one-key label keeps its box square rather than turning into a
        // sliver.
        let bw =
            (draw::text_width(font, &h.label.to_ascii_uppercase(), px) as i32 + 2 * pad_x).max(bh);
        let spots = [
            (ex, ey - bh / 2),
            (ex + ew - bw, ey - bh / 2),
            (ex, ey + eh - bh / 2),
            (ex + ew - bw, ey + eh - bh / 2),
            (ex - bw, ey + (eh - bh) / 2),
            (ex + ew, ey + (eh - bh) / 2),
        ];
        let fit = |(x, y): (i32, i32)| LabelBox {
            x: x.clamp(0, (canvas.0 - bw).max(0)),
            y: y.clamp(0, (canvas.1 - bh).max(0)),
            w: bw,
            h: bh,
        };
        let chosen = spots
            .into_iter()
            .map(fit)
            .find(|b| !taken.iter().any(|t| t.crowds(b, gap)))
            .unwrap_or_else(|| fit(spots[0]));
        taken.push(chosen);
    }
    taken
}

/// What hint mode has to show: the hints themselves, the keys confirmed so
/// far, and the key armed on top of them.
struct HintView<'a> {
    hints: &'a [Hint],
    typed: &'a str,
    armed: Option<char>,
}

fn draw_pick_hint(
    snap: &Snapshot,
    win: usize,
    view: HintView,
    font: &Font,
    canvas: &mut Canvas,
    scale: i32,
) {
    let HintView {
        hints,
        typed,
        armed,
    } = view;
    // What the armed key would leave behind, which is what it previews.
    let preview = armed.map(|ch| format!("{typed}{ch}"));
    // One candidate left means confirming that key clicks, so it is worth
    // more than the green a narrowing press gets.
    let clinches = preview.as_deref().is_some_and(|p| {
        hints
            .iter()
            .filter(|h| h.label.starts_with(p))
            .take(2)
            .count()
            == 1
    });
    let mon = &snap.monitor;
    let w = &snap.windows[win];
    let s = scale as f32;
    let px = LABEL_PX * s;
    canvas.round_rect_outline(
        Rect::new(
            ((w.x - mon.x) * scale) as f32,
            ((w.y - mon.y) * scale) as f32,
            (w.w * scale) as f32,
            (w.h * scale) as f32,
        )
        .grow(-s),
        8.0 * s,
        1.5 * s,
        TILE_BORDER,
    );
    let boxes = place_labels(hints, font, (mon.x, mon.y), (canvas.w, canvas.h), scale);
    // What the armed key keeps is drawn last so nothing can cover it.
    for pass_hot in [false, true] {
        for (h, b) in hints.iter().zip(&boxes) {
            if !h.label.starts_with(typed) {
                continue;
            }
            let hot = preview.as_deref().is_some_and(|p| h.label.starts_with(p));
            if hot != pass_hot {
                continue;
            }
            // While a key is armed, what it rules out steps well back rather
            // than competing with what it keeps.
            let dulled = preview.is_some() && !hot;
            let fade = if dulled { 0.18 } else { 1.0 };
            let (ring, bg, edge, text) = if hot {
                (ARMED_RING, ARMED_BG, ARMED_EDGE, ARMED_TEXT)
            } else {
                (
                    HINT_RING.fade(fade),
                    HINT_BG.fade(fade),
                    HINT_EDGE.fade(fade),
                    HINT_TEXT.fade(fade),
                )
            };
            // Outlining every element at once is noise; the outline only
            // earns its place once a key has narrowed the field.
            if hot || !typed.is_empty() {
                let el = Rect::new(
                    ((h.rx - mon.x as f64) * scale as f64) as f32,
                    ((h.ry - mon.y as f64) * scale as f64) as f32,
                    (h.rw * scale as f64) as f32,
                    (h.rh * scale as f64) as f32,
                );
                if hot && clinches {
                    canvas.round_rect_shadow(el, 4.0 * s, 6.0 * s, ARMED_GLOW.fade(0.45));
                }
                let t = if hot && clinches { 2.0 * s } else { s };
                canvas.round_rect_outline(el, 4.0 * s, t, ring);
            }
            // An armed label grows a little, on top of turning green.
            let r = if hot {
                b.rect().grow(1.5 * s)
            } else {
                b.rect()
            };
            let radius = r.h * 0.3;
            if hot && clinches {
                canvas.round_rect_shadow(r, radius, 7.0 * s, ARMED_GLOW);
            } else if !dulled {
                canvas.round_rect_shadow(r.shift(0.0, 1.2 * s), radius, 3.5 * s, SHADOW);
            }
            canvas.round_rect(r, radius, bg);
            canvas.round_rect_outline(r, radius, s, edge);
            draw_label_text(canvas, font, r, &h.label, typed.len(), hot, px, text, s);
        }
    }
    let pending = Pending {
        typed,
        armed,
        clinches,
    };
    draw_hud(canvas, font, window_rect(snap, win, scale), pending, s);
}

/// The label's own text: keys already confirmed step back, the armed key
/// wears a pressed cap, and what is still to come stays plain.
#[allow(clippy::too_many_arguments)]
fn draw_label_text(
    canvas: &mut Canvas,
    font: &Font,
    r: Rect,
    label: &str,
    done: usize,
    hot: bool,
    px: f32,
    text: Color,
    s: f32,
) {
    let label = label.to_ascii_uppercase();
    let base = draw::baseline(font, r.y + r.h / 2.0, px);
    let pen = r.x + (r.w - draw::text_width(font, &label, px)) / 2.0;
    let (done, rest) = label.split_at(done);
    let pen = canvas.text_run(font, done, pen, base, px, text.fade(0.35));
    if !hot {
        canvas.text_run(font, rest, pen, base, px, text);
        return;
    }
    // The armed key is the first one left. Showing it pressed, rather than
    // only tinting the whole label, is what says a key is waiting on a
    // second press.
    let (armed, tail) = rest.split_at(rest.chars().next().map_or(0, char::len_utf8));
    let cap = Rect::new(
        pen - 1.5 * s,
        r.y + 2.5 * s,
        draw::text_width(font, armed, px) + 3.0 * s,
        r.h - 5.0 * s,
    );
    canvas.round_rect(cap, cap.h * 0.3, ARMED_CAP);
    let pen = canvas.text_run(font, armed, pen, base, px, ARMED_CAP_TEXT);
    canvas.text_run(font, tail, pen, base, px, text);
}

/// The window being hinted, in buffer pixels.
fn window_rect(snap: &Snapshot, win: usize, scale: i32) -> Rect {
    let (mon, w) = (&snap.monitor, &snap.windows[win]);
    Rect::new(
        ((w.x - mon.x) * scale) as f32,
        ((w.y - mon.y) * scale) as f32,
        (w.w * scale) as f32,
        (w.h * scale) as f32,
    )
}

/// A strip along the bottom of the window naming the keys confirmed so far,
/// the key waiting on a second press, and what that press will do. Two
/// presses per key only works if the first one visibly asks a question
/// instead of looking like an answer.
/// The key press that has not been confirmed yet, and what confirming it
/// would do.
struct Pending<'a> {
    typed: &'a str,
    armed: Option<char>,
    clinches: bool,
}

fn draw_hud(canvas: &mut Canvas, font: &Font, win: Rect, pending: Pending, s: f32) {
    let Pending {
        typed,
        armed,
        clinches,
    } = pending;
    if armed.is_none() && typed.is_empty() {
        return;
    }
    let px = 15.0 * s;
    let cap = px * 1.75;
    let gap = 6.0 * s;
    let pad = 14.0 * s;
    let keys: Vec<(char, bool)> = typed
        .chars()
        .map(|c| (c, false))
        .chain(armed.map(|c| (c, true)))
        .collect();
    let msg = match (armed.is_some(), clinches) {
        (true, true) => "press again to click",
        (true, false) => "press again to confirm",
        (false, _) => "confirmed, next key",
    };
    let keys_w = keys.len() as f32 * (cap + gap) + gap;
    let panel = Rect::new(
        0.0,
        0.0,
        pad * 2.0 + keys_w + draw::text_width(font, msg, px),
        cap + pad,
    );
    let panel = Rect::new(
        (win.x + (win.w - panel.w) / 2.0).clamp(0.0, (canvas.w as f32 - panel.w).max(0.0)),
        (win.y + win.h - panel.h - 28.0 * s).clamp(0.0, (canvas.h as f32 - panel.h).max(0.0)),
        panel.w,
        panel.h,
    );
    let radius = panel.h * 0.32;
    canvas.round_rect_shadow(panel.shift(0.0, 2.0 * s), radius, 10.0 * s, SHADOW);
    canvas.round_rect(panel, radius, BOX_BG);
    canvas.round_rect_outline(panel, radius, s, BOX_BORDER.fade(0.35));
    let mut x = panel.x + pad;
    for (ch, hot) in keys {
        let key = Rect::new(x, panel.y + (panel.h - cap) / 2.0, cap, cap);
        canvas.round_rect(key, cap * 0.26, if hot { ARMED_BG } else { HUD_KEY });
        canvas.round_rect_outline(
            key,
            cap * 0.26,
            s,
            if hot {
                ARMED_RING
            } else {
                BOX_BORDER.fade(0.4)
            },
        );
        canvas.text_centered(
            font,
            &ch.to_ascii_uppercase().to_string(),
            key.x + cap / 2.0,
            key.y + cap / 2.0,
            px,
            if hot { ARMED_TEXT } else { TEXT.fade(0.75) },
        );
        x += cap + gap;
    }
    canvas.text_run(
        font,
        msg,
        x + gap,
        draw::baseline(font, panel.y + panel.h / 2.0, px),
        px,
        TEXT.fade(0.9),
    );
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        (self.width, self.height) = configure.new_size;
        if self.width == 0 || self.height == 0 {
            self.width = self.snap.monitor.logical_w as u32;
            self.height = self.snap.monitor.logical_h as u32;
        }
        self.configured = true;
        self.dirty = true;
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            if let Ok(kbd) = self.seat_state.get_keyboard(qh, &seat, None) {
                self.keyboard = Some(kbd);
            }
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: Capability,
    ) {
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.handle_key(event);
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: u32,
    ) {
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_keyboard!(App);
delegate_layer!(App);
delegate_registry!(App);
wayland_client::delegate_noop!(App: ignore ZwlrVirtualPointerManagerV1);
wayland_client::delegate_noop!(App: ignore ZwlrVirtualPointerV1);
