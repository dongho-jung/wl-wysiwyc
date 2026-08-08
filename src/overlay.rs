use crate::atspi::{self, Element};
use crate::draw::{self, Canvas, Color};
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

const DIM: Color = Color::new(0.0, 0.0, 0.0, 0.22);
const BOX_BG: Color = Color::new(0.08, 0.08, 0.10, 0.72);
const BOX_BORDER: Color = Color::new(1.0, 1.0, 1.0, 0.85);
const TEXT: Color = Color::new(1.0, 1.0, 1.0, 0.95);
const TILE_BG: Color = Color::new(0.08, 0.08, 0.10, 0.28);
const TILE_BORDER: Color = Color::new(1.0, 1.0, 1.0, 0.45);
const HINT_BG: Color = Color::new(0.99, 0.76, 0.16, 0.96);
const HINT_EDGE: Color = Color::new(0.35, 0.24, 0.02, 0.9);
const HINT_TEXT: Color = Color::new(0.12, 0.08, 0.0, 1.0);
const HINT_RING: Color = Color::new(1.0, 0.85, 0.3, 0.5);

pub struct Smoke {
    pub duration: Duration,
    /// 1-based window index; when set, render the letter grid for that
    /// window instead of the window picker.
    pub grid_window: Option<usize>,
    /// 1-based window index; when set, query clickable elements and render
    /// the hint overlay for that window.
    pub hints_window: Option<usize>,
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
    typed: String,
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

    let stage = match smoke.as_ref().and_then(|s| s.grid_window) {
        Some(n) if n >= 1 && n <= snap.windows.len() => Stage::PickTile { win: n - 1 },
        _ => Stage::PickWindow,
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
        elements_cache: HashMap::new(),
        pending_pick: None,
    };

    // Output names arrive with the initial burst of globals metadata.
    queue.roundtrip(&mut app)?;
    let output = app.find_output();

    if let Some(n) = smoke.as_ref().and_then(|s| s.hints_window) {
        if (1..=app.snap.windows.len()).contains(&n) {
            app.pending_pick = Some(n - 1);
            app.process_pending_pick();
        }
    }

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
        if event.keysym == Keysym::Escape {
            match self.stage {
                Stage::PickHint { .. } if !self.typed.is_empty() => {
                    self.typed.clear();
                    self.dirty = true;
                }
                Stage::PickTile { .. } | Stage::PickHint { .. } => {
                    self.stage = Stage::PickWindow;
                    self.typed.clear();
                    self.dirty = true;
                }
                Stage::PickWindow => self.exit = true,
            }
            return;
        }
        if event.keysym == Keysym::BackSpace {
            if matches!(self.stage, Stage::PickHint { .. }) && self.typed.pop().is_some() {
                self.dirty = true;
            }
            return;
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
                if ch.is_ascii_lowercase() {
                    let w = &self.snap.windows[win];
                    if let Some(t) = grid::tile_for(w.w as f64, w.h as f64, ch) {
                        self.target =
                            Some((w.x as f64 + t.x + t.w / 2.0, w.y as f64 + t.y + t.h / 2.0));
                        self.exit = true;
                    }
                }
            }
            Stage::PickHint { win } => {
                if ch == ' ' {
                    self.stage = Stage::PickTile { win };
                    self.typed.clear();
                    self.dirty = true;
                    return;
                }
                if !ch.is_ascii_lowercase() {
                    return;
                }
                self.typed.push(ch);
                let mut prefix_matches = 0;
                let mut exact: Option<(f64, f64)> = None;
                for h in &self.hints {
                    if h.label.starts_with(&self.typed) {
                        prefix_matches += 1;
                        if h.label == self.typed {
                            exact = Some((h.cx, h.cy));
                        }
                    }
                }
                if let Some(t) = exact {
                    self.target = Some(t);
                    self.exit = true;
                } else if prefix_matches == 0 {
                    self.typed.clear();
                    self.dirty = true;
                } else {
                    self.dirty = true;
                }
            }
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
            self.typed.clear();
            self.dirty = true;
        } else {
            self.enter_hint_stage(win);
        }
    }

    fn enter_hint_stage(&mut self, win: usize) {
        let els = self.elements_cache.get(&win).cloned().unwrap_or_default();
        let w = &self.snap.windows[win];
        let centers: Vec<(f64, f64)> = els
            .iter()
            .map(|e| (e.x + e.w / 2.0, e.y + e.h / 2.0))
            .collect();
        self.hints = els
            .iter()
            .zip(centers.iter())
            .zip(hint::labels(&centers))
            .map(|((e, &(cx, cy)), label)| Hint {
                label,
                rx: w.x as f64 + e.x,
                ry: w.y as f64 + e.y,
                rw: e.w,
                rh: e.h,
                cx: w.x as f64 + cx,
                cy: w.y as f64 + cy,
            })
            .collect();
        self.typed.clear();
        self.stage = Stage::PickHint { win };
        self.dirty = true;
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
                draw_pick_tile(&self.snap, win, &self.font, &mut canvas, scale)
            }
            Stage::PickHint { win } => draw_pick_hint(
                &self.snap,
                win,
                &self.hints,
                &self.typed,
                &self.font,
                &mut canvas,
                scale,
            ),
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

fn draw_pick_window(snap: &Snapshot, font: &Font, canvas: &mut Canvas, scale: i32) {
    let mon = &snap.monitor;
    for (i, w) in snap.windows.iter().enumerate() {
        let rx = (w.x - mon.x) * scale;
        let ry = (w.y - mon.y) * scale;
        let rw = w.w * scale;
        let rh = w.h * scale;
        canvas.stroke_rect(rx, ry, rw, rh, 2 * scale, TILE_BORDER);
        let side = ((w.w.min(w.h) as f64 * 0.30).clamp(56.0, 140.0) as i32) * scale;
        let cx = rx + rw / 2;
        let cy = ry + rh / 2;
        canvas.fill_rect(cx - side / 2, cy - side / 2, side, side, BOX_BG);
        canvas.stroke_rect(cx - side / 2, cy - side / 2, side, side, 2 * scale, BOX_BORDER);
        canvas.text_centered(
            font,
            &(i + 1).to_string(),
            cx as f32,
            cy as f32,
            side as f32 * 0.55,
            TEXT,
        );
    }
}

fn draw_pick_tile(snap: &Snapshot, win: usize, font: &Font, canvas: &mut Canvas, scale: i32) {
    let mon = &snap.monitor;
    let w = &snap.windows[win];
    let rx = (w.x - mon.x) * scale;
    let ry = (w.y - mon.y) * scale;
    let rw = w.w * scale;
    let rh = w.h * scale;
    for t in grid::tiles(rw as f64, rh as f64) {
        let tx = rx + t.x.round() as i32;
        let ty = ry + t.y.round() as i32;
        let tw = t.w.round() as i32;
        let th = t.h.round() as i32;
        canvas.fill_rect(tx, ty, tw, th, TILE_BG);
        canvas.stroke_rect(tx, ty, tw, th, scale.max(1), TILE_BORDER);
        let px = (t.w.min(t.h) as f32 * 0.42).min(64.0 * scale as f32);
        canvas.text_centered(
            font,
            &t.ch.to_ascii_uppercase().to_string(),
            tx as f32 + tw as f32 / 2.0,
            ty as f32 + th as f32 / 2.0,
            px,
            TEXT,
        );
    }
}

fn draw_pick_hint(
    snap: &Snapshot,
    win: usize,
    hints: &[Hint],
    typed: &str,
    font: &Font,
    canvas: &mut Canvas,
    scale: i32,
) {
    let mon = &snap.monitor;
    let w = &snap.windows[win];
    canvas.stroke_rect(
        (w.x - mon.x) * scale,
        (w.y - mon.y) * scale,
        w.w * scale,
        w.h * scale,
        2 * scale,
        TILE_BORDER,
    );
    let px = 15.0 * scale as f32;
    let pad = 4 * scale;
    for h in hints {
        if !h.label.starts_with(typed) {
            continue;
        }
        let ex = ((h.rx - mon.x as f64) * scale as f64) as i32;
        let ey = ((h.ry - mon.y as f64) * scale as f64) as i32;
        let ew = (h.rw * scale as f64) as i32;
        let eh = (h.rh * scale as f64) as i32;
        canvas.stroke_rect(ex, ey, ew, eh, scale.max(1), HINT_RING);
        let rem = h.label[typed.len()..].to_ascii_uppercase();
        let bw = draw::text_width(font, &rem, px) as i32 + 2 * pad;
        let bh = px as i32 + 2 * pad;
        let bx = ex.clamp(0, (canvas.w - bw).max(0));
        let by = (ey - bh / 2).clamp(0, (canvas.h - bh).max(0));
        canvas.fill_rect(bx, by, bw, bh, HINT_BG);
        canvas.stroke_rect(bx, by, bw, bh, scale.max(1), HINT_EDGE);
        canvas.text_centered(
            font,
            &rem,
            bx as f32 + bw as f32 / 2.0,
            by as f32 + bh as f32 / 2.0,
            px,
            HINT_TEXT,
        );
    }
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
