use crate::atspi::{self, Element};
use crate::config;
use crate::draw::{self, Canvas, Color, Rect};
use crate::grid;
use crate::hint;
use crate::hypr::{self, Snapshot};
use crate::shortcuts::{
    protocol::hyprland_global_shortcut_v1::{self, HyprlandGlobalShortcutV1},
    protocol::hyprland_global_shortcuts_manager_v1::HyprlandGlobalShortcutsManagerV1,
    Key, Shortcuts,
};
use fontdue::Font;
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
use std::collections::HashMap;
use std::error::Error;
use std::io::ErrorKind;
use std::time::{Duration, Instant};
use wayland_client::{
    backend::WaylandError,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_keyboard, wl_output, wl_pointer, wl_region, wl_registry, wl_seat, wl_shm, wl_surface,
    },
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

/// Colours the config does not name, because they are shades of ones it
/// does: the darker edge of a label, the wash and glow around an armed
/// target, and the panel behind a window number.
fn hint_edge() -> Color {
    shade(*config::get().colors.hint_text, 0.35)
}
fn armed_edge() -> Color {
    shade(*config::get().colors.armed_text, 0.55)
}
fn armed_glow() -> Color {
    shade(*config::get().colors.ring, 0.5)
}
fn armed_fill() -> Color {
    shade(*config::get().colors.armed, 0.14)
}
fn box_bg() -> Color {
    Color::new(0.09, 0.09, 0.12, 0.86)
}
fn box_border() -> Color {
    shade(*config::get().colors.text, 0.55)
}

/// The same colour at a set opacity, whatever the original had.
fn shade(c: Color, a: f32) -> Color {
    Color::new(c.r, c.g, c.b, a)
}

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

/// The pointer on its way to a target. Snapping it there would be quicker,
/// but a pointer that jumps leaves you hunting for it; one that travels tells
/// you where it went and roughly how far.
struct Glide {
    from: (f64, f64),
    to: (f64, f64),
    started: Instant,
}

impl Glide {
    /// How long the trip takes. Long enough to follow, short enough not to
    /// wait for.
    const TIME: Duration = Duration::from_millis(110);

    /// Where the pointer should be now, and whether it has arrived.
    fn at(&self) -> ((f64, f64), bool) {
        let t = (self.started.elapsed().as_secs_f64() / Self::TIME.as_secs_f64()).min(1.0);
        // Ease out: quick off the mark, gentle into the target.
        let e = 1.0 - (1.0 - t).powi(3);
        let x = self.from.0 + (self.to.0 - self.from.0) * e;
        let y = self.from.1 + (self.to.1 - self.from.1) * e;
        ((x, y), t >= 1.0)
    }
}

/// What a complete hint or tile picked out.
#[derive(Clone, Copy, PartialEq)]
enum Target {
    Hint(usize),
    Tile(char),
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
    /// Where to click on the way out, with which button, and how many times.
    target: Option<(f64, f64)>,
    button: u32,
    clicks: u32,
    /// Where to send the pointer, once the loop can get to it.
    aim: Option<(f64, f64)>,
    hints: Vec<Hint>,
    /// Keys already confirmed, narrowing the hints.
    typed: String,
    /// Whether shift is held, which turns the left click key into a double
    /// click. Only the fallback path needs this; a global shortcut arrives
    /// already told apart by the compositor.
    shift: bool,
    /// With `keys.confirm` on, a key pressed once and not taken yet: it shows
    /// what it would select, and pressing it again takes it.
    armed: Option<char>,
    /// The target a complete hint or tile picked out, waiting for a click
    /// key. The pointer is already sitting on it.
    picked: Option<(Target, (f64, f64))>,
    elements_cache: HashMap<usize, Vec<Element>>,
    pending_pick: Option<usize>,
    /// An element query already in flight, so the wait for it overlaps the
    /// rest of the startup instead of following it.
    query: Option<Query>,
}

/// An element query running on its own thread.
struct Query {
    win: usize,
    rx: std::sync::mpsc::Receiver<Result<Vec<Element>, String>>,
    started: Instant,
}

impl Query {
    fn start(snap: &Snapshot, win: usize) -> Self {
        let w = &snap.windows[win];
        let (pid, title) = (w.pid, w.title.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(atspi::clickable_elements(pid, &title).map_err(|e| e.to_string()));
        });
        Self {
            win,
            rx,
            started: Instant::now(),
        }
    }

    /// Wait for the elements, giving up on an application that never answers.
    fn take(self) -> Vec<Element> {
        let wait = Duration::from_millis(config::get().elements.query_ms)
            .saturating_sub(self.started.elapsed());
        match self.rx.recv_timeout(wait) {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                eprintln!("atspi: {e}");
                Vec::new()
            }
            Err(_) => {
                eprintln!("atspi: element query timed out");
                Vec::new()
            }
        }
    }
}

/// Show the overlay and return the chosen global click position, or None
/// if the user cancelled. The click itself is performed here as well,
/// after the overlay is torn down.
pub fn run(snap: Snapshot, smoke: Option<Smoke>) -> Result<Option<(f64, f64)>, Box<dyn Error>> {
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

    // Ask the window for its elements before doing anything else. It is the
    // slowest part of starting up by a distance, and everything below - the
    // font, the Wayland globals, the surface - happens while it runs.
    let query = pending_pick.map(|win| Query::start(&snap, win));

    let font = draw::load_font()?;
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<App>(&conn)?;
    let qh = queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)?;
    let layer_shell = LayerShell::bind(&globals, &qh)?;
    let shm = Shm::bind(&globals, &qh)?;
    let pool = SlotPool::new(4096, &shm)?;
    let vp_manager: Option<ZwlrVirtualPointerManagerV1> = globals.bind(&qh, 1..=2, ()).ok();

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
        button: BTN_LEFT,
        clicks: 1,
        aim: None,
        hints: Vec::new(),
        typed: String::new(),
        shift: false,
        armed: None,
        picked: None,
        elements_cache: HashMap::new(),
        pending_pick,
        query,
    };

    // Output names arrive with the initial burst of globals metadata.
    queue.roundtrip(&mut app)?;
    let output = app.find_output();

    // Read the window before taking any keys. The read can take a second on
    // a heavy page, and it is better spent while the keyboard still belongs
    // to the user than inside a submap that cannot answer yet.
    app.process_pending_pick();

    let surface = compositor.create_surface(&qh);
    // Take no pointer input at all. A surface that accepts it pulls the
    // pointer off whatever is underneath, and the window sees the pointer
    // leave: menus that hang on hover fold up as the overlay opens, and the
    // hints then describe a window that is no longer there. Keyboard focus
    // comes from the layer's interactivity, not from this, so nothing is
    // lost by giving it up.
    let empty = compositor.wl_compositor().create_region(&qh, ());
    surface.set_input_region(Some(&empty));
    empty.destroy();
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
    // Keys come from the compositor as global shortcuts when it speaks that
    // protocol. Holding the keyboard ourselves is the fallback, and it costs
    // the window its activation, which costs it any menu open on hover.
    let shortcuts = match smoke {
        Some(_) => None,
        None => Shortcuts::bind(&globals, &qh),
    };
    layer.set_keyboard_interactivity(if smoke.is_some() || shortcuts.is_some() {
        KeyboardInteractivity::None
    } else {
        KeyboardInteractivity::Exclusive
    });
    layer.commit();
    app.layer = Some(layer);

    // One virtual pointer for the whole run: it sends the pointer to a picked
    // target while the overlay is up, and clicks once it comes down.
    let pointer = vp_manager.as_ref().map(|mgr| {
        let seat = app.seat_state.seats().next();
        mgr.create_virtual_pointer(seat.as_ref(), &qh, ())
    });

    // Where the pointer is, so a target can be travelled to rather than
    // jumped to.
    let mut pointer_at = hypr::cursor_pos();
    let mut glide: Option<Glide> = None;

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
        let start = Instant::now();
        while !app.exit {
            // A surface that never gets configured means something else owns
            // the screen. Give up rather than sit in the submap holding the
            // keyboard hostage for a window that will not appear.
            if !app.configured && start.elapsed() > Duration::from_secs(3) {
                return Err("compositor never configured the overlay".into());
            }
            // Wait on the compositor, or on the next step of a glide,
            // whichever comes first.
            queue.flush()?;
            if let Some(guard) = queue.prepare_read() {
                let wait = if glide.is_some() {
                    Duration::from_millis(8)
                } else {
                    Duration::from_secs(1)
                };
                if readable(&conn, wait) {
                    match guard.read() {
                        // Woken with nothing to read: the wakeup was spurious,
                        // or someone else drained the socket.
                        Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
                        other => {
                            other?;
                        }
                    }
                }
            }
            queue.dispatch_pending(&mut app)?;
            app.process_pending_pick();
            // A picked target wants the pointer on it. Doing that here rather
            // than where it was picked keeps the requests and their roundtrip
            // out of the middle of an event callback.
            if let Some(to) = app.aim.take() {
                glide = match pointer_at {
                    Some(from) => Some(Glide {
                        from,
                        to,
                        started: Instant::now(),
                    }),
                    // Nowhere to travel from, so just be there.
                    None => {
                        pointer_at = Some(to);
                        None
                    }
                };
                if glide.is_none() {
                    if let Some(vp) = pointer.as_ref() {
                        let extent = app.snap.layout_extent;
                        move_and_click(vp, to, extent, None, &mut queue, &mut app)?;
                    }
                }
            }
            if let (Some(g), Some(vp)) = (glide.as_ref(), pointer.as_ref()) {
                let (at, done) = g.at();
                let extent = app.snap.layout_extent;
                move_and_click(vp, at, extent, None, &mut queue, &mut app)?;
                pointer_at = Some(at);
                if done {
                    glide = None;
                }
            }
            if !app.exit && app.configured && app.dirty {
                app.draw();
            }
        }
    }

    // Unmap the overlay before injecting the click so the click reaches
    // the window below instead of this surface. Dropping the shortcuts first
    // puts the keyboard back the way it was.
    drop(shortcuts);
    app.layer.take();
    queue.roundtrip(&mut app)?;

    if let Some(target) = app.target {
        let vp = pointer
            .as_ref()
            .ok_or("compositor does not expose zwlr_virtual_pointer_manager_v1")?;
        let extent = app.snap.layout_extent;
        let click = (app.button, app.clicks);
        move_and_click(vp, target, extent, Some(click), &mut queue, &mut app)?;
        vp.destroy();
        queue.roundtrip(&mut app)?;
    }
    Ok(app.target)
}

/// Wait for the compositor to have something to say, or for the timeout to
/// run out, whichever happens first.
fn readable(conn: &Connection, timeout: Duration) -> bool {
    use std::os::fd::{AsFd, AsRawFd};
    let mut poll = libc::pollfd {
        fd: conn.as_fd().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    // SAFETY: one initialized pollfd, and a length that matches it.
    unsafe { libc::poll(&mut poll, 1, ms) > 0 }
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
    move_and_click(&vp, target, snap.layout_extent, None, &mut queue, &mut mini)?;
    vp.destroy();
    queue.roundtrip(&mut mini)?;
    Ok(())
}

fn move_and_click<S>(
    vp: &ZwlrVirtualPointerV1,
    (gx, gy): (f64, f64),
    (ext_w, ext_h): (i32, i32),
    click: Option<(u32, u32)>,
    queue: &mut wayland_client::EventQueue<S>,
    state: &mut S,
) -> Result<(), Box<dyn Error>> {
    let x = gx.round().clamp(0.0, (ext_w - 1) as f64) as u32;
    let y = gy.round().clamp(0.0, (ext_h - 1) as f64) as u32;
    vp.motion_absolute(0, x, y, ext_w as u32, ext_h as u32);
    vp.frame();
    queue.roundtrip(state)?;
    if let Some((button, times)) = click {
        for n in 0..times.max(1) {
            // A pause the toolkit underneath can tell apart: too quick and
            // the two presses of a double click arrive as one.
            std::thread::sleep(Duration::from_millis(if n == 0 { 20 } else { 40 }));
            vp.button(0, button, wl_pointer::ButtonState::Pressed);
            vp.frame();
            vp.button(0, button, wl_pointer::ButtonState::Released);
            vp.frame();
            queue.roundtrip(state)?;
        }
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

    fn press(&mut self, key: Key) {
        let ch = match key {
            Key::Escape => return self.cancel(),
            Key::Backspace => return self.undo(),
            Key::Tab => return self.pick_window(),
            Key::LeftClick => return self.click_picked(BTN_LEFT, 1),
            Key::RightClick => return self.click_picked(BTN_RIGHT, 1),
            Key::DoubleClick => return self.click_picked(BTN_LEFT, 2),
            Key::Reset => return self.reset_input(),
            Key::Left => return self.step(-1.0, 0.0),
            Key::Right => return self.step(1.0, 0.0),
            Key::Up => return self.step(0.0, -1.0),
            Key::Down => return self.step(0.0, 1.0),
            Key::Char(ch) => ch.to_ascii_lowercase(),
        };
        if ch == ' ' {
            return self.switch_mode();
        }
        if config::get().keys.confirm {
            // Every key is shown before it is taken: the first press arms it,
            // the same key again takes it, and any other key aims elsewhere.
            if self.armed == Some(ch) {
                self.armed = None;
                self.take(ch);
            } else if self.leads_anywhere(ch) {
                self.armed = Some(ch);
                self.dirty = true;
            }
            return;
        }
        self.take(ch);
    }

    /// Act on a key, now that it is meant.
    fn take(&mut self, ch: char) {
        self.dirty = true;
        match self.stage {
            Stage::PickWindow => {
                if let Some(d) = ch.to_digit(10) {
                    let idx = d as usize;
                    if (1..=self.snap.windows.len()).contains(&idx) {
                        // Act on the pick here rather than back in the event
                        // loop: reading the window's elements takes long
                        // enough for the next key to arrive, and it would be
                        // read against the stage this one is leaving.
                        self.pending_pick = Some(idx - 1);
                        self.process_pending_pick();
                    }
                }
            }
            Stage::PickTile { win } => {
                let w = &self.snap.windows[win];
                if let Some(t) = grid::tile_for(w.w as f64, w.h as f64, ch) {
                    let at = (w.x as f64 + t.x + t.w / 2.0, w.y as f64 + t.y + t.h / 2.0);
                    self.pick(Target::Tile(ch), at);
                }
            }
            Stage::PickHint { .. } => {
                if !self.leads_anywhere(ch) {
                    return;
                }
                self.typed.push(ch);
                let done = self
                    .hints
                    .iter()
                    .position(|h| h.label == self.typed)
                    .map(|i| (i, (self.hints[i].cx, self.hints[i].cy)));
                if let Some((i, at)) = done {
                    self.pick(Target::Hint(i), at);
                }
            }
        }
    }

    /// A hint or tile is complete. Either click it straight away, or take it
    /// as the target and wait to be told which button, which is what makes
    /// the overlay usable for a hover or a right click as well as a click.
    fn pick(&mut self, what: Target, at: (f64, f64)) {
        if config::get().keys.instant {
            self.target = Some(at);
            self.button = BTN_LEFT;
            self.exit = true;
            return;
        }
        self.focus(what, at);
    }

    /// Take a target without clicking it, wherever it came from: a hint typed
    /// out, an arrow key, or the pointer's own position when the overlay
    /// opened.
    fn focus(&mut self, what: Target, at: (f64, f64)) {
        // Typing is done with, so every hint comes back into view and another
        // one can be picked without backing out first.
        self.typed.clear();
        self.armed = None;
        self.picked = Some((what, at));
        // The overlay takes no pointer input, so sending the pointer here
        // lands it on the window underneath: whatever is under it lights up
        // the way it would if the pointer had been dragged there.
        self.aim = Some(at);
        self.dirty = true;
    }

    /// Every target the current mode offers, with where clicking it would
    /// land.
    fn targets(&self) -> Vec<(Target, (f64, f64))> {
        match self.stage {
            Stage::PickHint { .. } => self
                .hints
                .iter()
                .enumerate()
                .map(|(i, h)| (Target::Hint(i), (h.cx, h.cy)))
                .collect(),
            Stage::PickTile { win } => {
                let w = &self.snap.windows[win];
                grid::tiles(w.w as f64, w.h as f64)
                    .into_iter()
                    .map(|t| {
                        (
                            Target::Tile(t.ch),
                            (w.x as f64 + t.x + t.w / 2.0, w.y as f64 + t.y + t.h / 2.0),
                        )
                    })
                    .collect()
            }
            Stage::PickWindow => Vec::new(),
        }
    }

    /// Move the target one step in a direction.
    ///
    /// Anything roughly in line with the current target wins first, nearest
    /// one taken, which is what walks a row of buttons or a column of list
    /// items the way you would expect. Only when there is nothing in line does
    /// the search widen to a cone, so leaving the end of a row still goes
    /// somewhere sensible rather than nowhere.
    fn step(&mut self, dx: f64, dy: f64) {
        /// How far off the line a target can be and still count as in line,
        /// in logical pixels: about a row of text.
        const IN_LINE: f64 = 24.0;

        let from = match self.picked {
            Some((_, at)) => at,
            None => match hypr::cursor_pos() {
                Some(at) => at,
                None => return,
            },
        };
        let here = self.picked.map(|(p, _)| p);
        let reach: Vec<(f64, f64, Target, (f64, f64))> = self
            .targets()
            .into_iter()
            .filter(|&(what, _)| here != Some(what))
            .filter_map(|(what, at)| {
                let (ox, oy) = (at.0 - from.0, at.1 - from.1);
                let along = ox * dx + oy * dy;
                let across = (ox * dy - oy * dx).abs();
                (along > 1.0 && across <= along).then_some((along, across, what, at))
            })
            .collect();
        let best = reach
            .iter()
            .filter(|&&(_, across, _, _)| across <= IN_LINE)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .or_else(|| {
                reach
                    .iter()
                    .min_by(|a, b| (a.0 + 3.0 * a.1).total_cmp(&(b.0 + 3.0 * b.1)))
            });
        if let Some(&(_, _, what, at)) = best {
            self.focus(what, at);
        }
    }

    /// Start on whatever the pointer is already nearest, so the arrow keys
    /// have something to move from and a click needs no typing at all.
    fn focus_nearest_cursor(&mut self) {
        let Some(from) = hypr::cursor_pos() else {
            return;
        };
        let nearest = self.targets().into_iter().min_by(|a, b| {
            let d = |at: (f64, f64)| (at.0 - from.0).powi(2) + (at.1 - from.1).powi(2);
            d(a.1).total_cmp(&d(b.1))
        });
        if let Some((what, at)) = nearest {
            self.focus(what, at);
        }
    }

    /// Click whatever was picked, and leave.
    fn click_picked(&mut self, button: u32, clicks: u32) {
        let Some((_, at)) = self.picked else {
            return;
        };
        self.target = Some(at);
        self.button = button;
        self.clicks = clicks;
        self.exit = true;
    }

    /// Space swaps element hints for the letter grid and back.
    fn switch_mode(&mut self) {
        match self.stage {
            Stage::PickTile { win } => {
                if !self.elements_cache.get(&win).is_none_or(Vec::is_empty) {
                    self.enter_hint_stage(win);
                }
            }
            Stage::PickHint { win } => {
                self.stage = Stage::PickTile { win };
                self.reset_input();
            }
            Stage::PickWindow => {}
        }
    }

    /// Esc unwinds one step at a time: the armed key, then what was picked,
    /// then everything typed, then quit.
    fn cancel(&mut self) {
        if self.armed.take().is_some() || self.picked.take().is_some() {
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

    /// Backspace undoes one key press.
    fn undo(&mut self) {
        if self.armed.take().is_some() || self.picked.take().is_some() || self.typed.pop().is_some()
        {
            self.dirty = true;
        }
    }

    fn reset_input(&mut self) {
        self.typed.clear();
        self.armed = None;
        self.picked = None;
        self.dirty = true;
    }

    fn pick_window(&mut self) {
        if matches!(self.stage, Stage::PickWindow) {
            return;
        }
        self.stage = Stage::PickWindow;
        self.reset_input();
    }

    /// Whether this key would select anything at all, so a key that means
    /// nothing here can be ignored rather than shown.
    fn leads_anywhere(&self, ch: char) -> bool {
        match self.stage {
            Stage::PickWindow => ch
                .to_digit(10)
                .is_some_and(|d| (1..=self.snap.windows.len()).contains(&(d as usize))),
            Stage::PickTile { win } => {
                let w = &self.snap.windows[win];
                grid::tile_for(w.w as f64, w.h as f64, ch).is_some()
            }
            Stage::PickHint { .. } => {
                let mut probe = self.typed.clone();
                probe.push(ch);
                self.hints.iter().any(|h| h.label.starts_with(&probe))
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
            let query = match self.query.take() {
                Some(q) if q.win == win => q,
                other => {
                    self.query = other;
                    Query::start(&self.snap, win)
                }
            };
            let els = query.take();
            self.elements_cache.insert(win, els);
        }
        if self.elements_cache.get(&win).is_none_or(Vec::is_empty) {
            self.stage = Stage::PickTile { win };
            self.reset_input();
        } else {
            self.enter_hint_stage(win);
        }
        self.focus_nearest_cursor();
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
        let (buffer, buf) = match self
            .pool
            .create_buffer(bw, bh, bw * 4, wl_shm::Format::Argb8888)
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!("buffer allocation failed: {e}");
                return;
            }
        };
        let mut canvas = Canvas { buf, w: bw, h: bh };
        canvas.clear(*config::get().colors.dim);
        match self.stage {
            Stage::PickWindow => draw_pick_window(&self.snap, &self.font, &mut canvas, scale),
            Stage::PickTile { win } => {
                let lit = match self.picked {
                    Some((Target::Tile(ch), _)) => Some(ch),
                    _ => self.armed,
                };
                draw_pick_tile(&self.snap, win, lit, &self.font, &mut canvas, scale)
            }
            Stage::PickHint { win } => {
                let view = HintView {
                    hints: &self.hints,
                    typed: &self.typed,
                    armed: self.armed,
                    picked: match self.picked {
                        Some((Target::Hint(i), _)) => Some(i),
                        _ => None,
                    },
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
        .zip(hint::labels(
            &centers,
            w.w as f64,
            w.h as f64,
            &config::get().keys.reserved_letters(),
        ))
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
    canvas.clear(*config::get().colors.dim);
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
            picked: None,
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
        canvas.round_rect_outline(frame, 8.0 * s, 1.5 * s, *config::get().colors.tile_border);
        let side = (w.w.min(w.h) as f32 * 0.30).clamp(56.0, 140.0) * s;
        let card = Rect::new(
            frame.x + (frame.w - side) / 2.0,
            frame.y + (frame.h - side) / 2.0,
            side,
            side,
        );
        canvas.round_rect_shadow(
            card.shift(0.0, 2.0 * s),
            side * 0.24,
            6.0 * s,
            *config::get().colors.shadow,
        );
        canvas.round_rect(card, side * 0.24, box_bg());
        canvas.round_rect_outline(card, side * 0.24, 1.5 * s, box_border());
        canvas.text_centered(
            font,
            &(i + 1).to_string(),
            card.x + side / 2.0,
            card.y + side / 2.0,
            side * 0.55,
            *config::get().colors.text,
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
            canvas.round_rect_shadow(tile, radius, 10.0 * s, armed_glow());
        }
        canvas.round_rect(
            tile,
            radius,
            if hot {
                armed_fill()
            } else {
                *config::get().colors.tile
            },
        );
        canvas.round_rect_outline(
            tile,
            radius,
            if hot { 2.5 * s } else { s },
            if hot {
                *config::get().colors.ring
            } else {
                *config::get().colors.tile_border
            },
        );
        let letter = t.ch.to_ascii_uppercase().to_string();
        let px = (t.w.min(t.h) as f32 * 0.42).min(64.0 * s);
        let (cx, cy) = (tile.x + tile.w / 2.0, tile.y + tile.h / 2.0);
        // The letter sits over whatever the window is showing, so it carries
        // its own shadow rather than trusting the background.
        canvas.text_centered(
            font,
            &letter,
            cx + s,
            cy + s,
            px,
            *config::get().colors.shadow,
        );
        canvas.text_centered(font, &letter, cx, cy, px, *config::get().colors.text);
    }
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

    /// How much of this label lands on a rectangle, in pixels.
    fn covers(&self, (x, y, w, h): (i32, i32, i32, i32)) -> i64 {
        let ox = (self.x + self.w).min(x + w) - self.x.max(x);
        let oy = (self.y + self.h).min(y + h) - self.y.max(y);
        if ox <= 0 || oy <= 0 {
            0
        } else {
            ox as i64 * oy as i64
        }
    }
}

/// Lay out one box per hint.
///
/// A label large enough to swallow its target goes beside it instead of over
/// it: a row of small icons is unusable when every icon is under a label.
/// Beside is not enough on its own though, since the icon next door is just
/// as worth seeing, so of the places that clear the labels already put down,
/// the one covering the least of everything else wins. Bigger elements keep
/// the vimium placement, a corner of the element, which costs them nothing.
/// Placement covers every hint, not just the visible ones, so labels stay put
/// while a prefix is typed.
fn place_labels(
    hints: &[Hint],
    font: &Font,
    mon: (i32, i32),
    canvas: (i32, i32),
    scale: i32,
) -> Vec<LabelBox> {
    let cfg = &config::get().label;
    let px = cfg.size * scale as f32;
    let pad_x = (cfg.pad_x * scale as f32) as i32;
    let pad_y = (cfg.pad_y * scale as f32) as i32;
    let gap = (cfg.gap * scale as f32) as i32;
    let rect_of = |h: &Hint| {
        (
            ((h.rx - mon.0 as f64) * scale as f64) as i32,
            ((h.ry - mon.1 as f64) * scale as f64) as i32,
            (h.rw * scale as f64) as i32,
            (h.rh * scale as f64) as i32,
        )
    };
    let elements: Vec<(i32, i32, i32, i32)> = hints.iter().map(rect_of).collect();
    let mut taken: Vec<LabelBox> = Vec::with_capacity(hints.len());
    for h in hints {
        let (ex, ey, ew, eh) = rect_of(h);
        let bh = px as i32 + 2 * pad_y;
        // A one-key label keeps its box square rather than turning into a
        // sliver.
        let bw = (label_width(font, &h.label.to_ascii_uppercase(), px, scale as f32) as i32
            + 2 * pad_x)
            .max(bh);
        let (mid_x, mid_y) = (ex + (ew - bw) / 2, ey + (eh - bh) / 2);
        let small = ew < 2 * bw || eh < 2 * bh;
        let spots = if small {
            // Small target: everything here clears it.
            [
                (ex + ew + gap, mid_y),
                (ex - bw - gap, mid_y),
                (mid_x, ey - bh - gap),
                (mid_x, ey + eh + gap),
                (ex + ew + gap, ey - bh - gap),
                (ex - bw - gap, ey + eh + gap),
            ]
        } else {
            [
                (ex, ey - bh / 2),
                (ex + ew - bw, ey - bh / 2),
                (ex, ey + eh - bh / 2),
                (ex + ew - bw, ey + eh - bh / 2),
                (ex - bw - gap, mid_y),
                (ex + ew + gap, mid_y),
            ]
        };
        let fit = |(x, y): (i32, i32)| LabelBox {
            x: x.clamp(0, (canvas.0 - bw).max(0)),
            y: y.clamp(0, (canvas.1 - bh).max(0)),
            w: bw,
            h: bh,
        };
        // Anything on top of a label already placed is out; among the rest,
        // take whichever hides the least of the window.
        let mut best: Option<(i64, LabelBox)> = None;
        for spot in spots {
            let b = fit(spot);
            if taken.iter().any(|t| t.crowds(&b, gap)) {
                continue;
            }
            let hidden: i64 = elements.iter().map(|&e| b.covers(e)).sum();
            if best.as_ref().is_none_or(|(worst, _)| hidden < *worst) {
                best = Some((hidden, b));
            }
            if hidden == 0 {
                break;
            }
        }
        taken.push(best.map(|(_, b)| b).unwrap_or_else(|| fit(spots[0])));
    }
    taken
}

/// What hint mode has to show: the hints themselves, the keys confirmed so
/// far, and the key armed on top of them.
struct HintView<'a> {
    hints: &'a [Hint],
    typed: &'a str,
    armed: Option<char>,
    /// The hint a completed label picked out, waiting for a click key.
    picked: Option<usize>,
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
        picked,
    } = view;
    // What the armed key would leave behind, which is what it previews.
    let preview = armed.map(|ch| format!("{typed}{ch}"));
    // One candidate left means the next press takes it, so it is worth more
    // than the green a narrowing press gets. A picked target is that, arrived
    // at: it stays lit while it waits for a click key.
    let clinches = picked.is_some()
        || preview.as_deref().is_some_and(|p| {
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
    let px = config::get().label.size * s;
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
        *config::get().colors.tile_border,
    );
    let boxes = place_labels(hints, font, (mon.x, mon.y), (canvas.w, canvas.h), scale);
    // What the armed key keeps is drawn last so nothing can cover it.
    for pass_hot in [false, true] {
        for (i, (h, b)) in hints.iter().zip(&boxes).enumerate() {
            if !h.label.starts_with(typed) {
                continue;
            }
            let hot =
                picked == Some(i) || preview.as_deref().is_some_and(|p| h.label.starts_with(p));
            if hot != pass_hot {
                continue;
            }
            // An armed key lifts the hints it would keep. It does not push
            // the others down: nothing has been confirmed yet, and dimming
            // them would answer a question the press has only asked.
            let (ring, bg, edge, text) = if hot {
                (
                    *config::get().colors.ring,
                    *config::get().colors.armed,
                    armed_edge(),
                    *config::get().colors.armed_text,
                )
            } else {
                (
                    shade(*config::get().colors.hint, 0.5),
                    *config::get().colors.hint,
                    hint_edge(),
                    *config::get().colors.hint_text,
                )
            };
            // Only the element about to be clicked is outlined. Ringing
            // every candidate turns a dense corner of a window into a mess
            // of boxes, and the labels already say which ones are left.
            if hot && clinches {
                let el = Rect::new(
                    ((h.rx - mon.x as f64) * scale as f64) as f32,
                    ((h.ry - mon.y as f64) * scale as f64) as f32,
                    (h.rw * scale as f64) as f32,
                    (h.rh * scale as f64) as f32,
                );
                canvas.round_rect_shadow(el, 4.0 * s, 6.0 * s, armed_glow().fade(0.45));
                canvas.round_rect_outline(el, 4.0 * s, 2.0 * s, ring);
            }
            let r = b.rect();
            let radius = r.h * 0.3;
            if hot && clinches {
                canvas.round_rect_shadow(r, radius, 7.0 * s, armed_glow());
            } else {
                canvas.round_rect_shadow(
                    r.shift(0.0, 1.2 * s),
                    radius,
                    3.5 * s,
                    *config::get().colors.shadow,
                );
            }
            canvas.round_rect(r, radius, bg);
            canvas.round_rect_outline(r, radius, s, edge);
            let cap = hot && armed.is_some();
            draw_label_text(canvas, font, r, &h.label, typed.len(), cap, px, text, s);
        }
    }
}

/// How wide a label's text is once its characters are spaced out.
fn label_width(font: &Font, label: &str, px: f32, s: f32) -> f32 {
    let n = label.chars().count().saturating_sub(1) as f32;
    draw::text_width(font, label, px) + n * config::get().label.track * s
}

/// The label's own text, character by character: keys already confirmed step
/// back, an armed key wears a pressed cap, and what is still to come stays
/// plain.
#[allow(clippy::too_many_arguments)]
fn draw_label_text(
    canvas: &mut Canvas,
    font: &Font,
    r: Rect,
    label: &str,
    done: usize,
    cap: bool,
    px: f32,
    text: Color,
    s: f32,
) {
    let label = label.to_ascii_uppercase();
    let track = config::get().label.track * s;
    let base = draw::baseline(font, r.y + r.h / 2.0, px);
    let mut pen = r.x + (r.w - label_width(font, &label, px, s)) / 2.0;
    for (i, ch) in label.chars().enumerate() {
        let glyph = ch.to_string();
        let w = draw::text_width(font, &glyph, px);
        // Only a key actually waiting on a second press wears the cap. A
        // label the pointer has already been sent to is not waiting on
        // anything, so capping its first character would say otherwise.
        let armed = cap && i == done;
        if armed {
            let cap = Rect::new(pen - track / 2.0, r.y + 2.5 * s, w + track, r.h - 5.0 * s);
            canvas.round_rect(cap, cap.h * 0.32, *config::get().colors.armed_key);
        }
        let color = match (armed, i < done) {
            (true, _) => *config::get().colors.armed_key_text,
            (_, true) => text.fade(0.35),
            _ => text,
        };
        canvas.text_run(font, &glyph, pen, base, px, color);
        pen += w + track;
    }
}

/// The key a wl_keyboard event stands for. Only used on the fallback path,
/// where the overlay holds the keyboard itself.
fn key_of(event: &KeyEvent) -> Option<Key> {
    let cfg = &config::get().keys;
    match event.keysym {
        Keysym::Escape => return Some(Key::Escape),
        Keysym::BackSpace => return Some(Key::Backspace),
        Keysym::Tab => return Some(Key::Tab),
        _ => {}
    }
    let name = keysym_name(event);
    if name == cfg.left() {
        return Some(Key::LeftClick);
    }
    if name == cfg.right() {
        return Some(Key::RightClick);
    }
    if cfg.reset().is_some_and(|k| k == name) {
        return Some(Key::Reset);
    }
    event
        .utf8
        .as_deref()
        .and_then(|s| s.chars().next())
        .map(Key::Char)
}

/// What the config would call this key. Enough of the keyboard to match a
/// click key by name; anything else is matched by the text it types.
fn keysym_name(event: &KeyEvent) -> String {
    let named = match event.keysym {
        Keysym::Return | Keysym::KP_Enter => "return",
        Keysym::space => "space",
        Keysym::minus | Keysym::KP_Subtract => "minus",
        Keysym::equal => "equal",
        Keysym::backslash => "backslash",
        Keysym::semicolon => "semicolon",
        Keysym::apostrophe => "apostrophe",
        Keysym::comma => "comma",
        Keysym::period => "period",
        Keysym::slash => "slash",
        Keysym::bracketleft => "bracketleft",
        Keysym::bracketright => "bracketright",
        Keysym::grave => "grave",
        Keysym::Hangul => "Hangul",
        _ => return event.utf8.clone().unwrap_or_default(),
    };
    named.to_string()
}

impl Dispatch<HyprlandGlobalShortcutV1, Key> for App {
    fn event(
        state: &mut Self,
        _: &HyprlandGlobalShortcutV1,
        event: hyprland_global_shortcut_v1::Event,
        key: &Key,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, hyprland_global_shortcut_v1::Event::Pressed { .. }) {
            state.press(*key);
        }
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
        if let Some(key) = key_of(&event) {
            let key = match (key, self.shift) {
                (Key::LeftClick, true) => Key::DoubleClick,
                (key, _) => key,
            };
            self.press(key);
        }
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
        modifiers: Modifiers,
        _: u32,
    ) {
        self.shift = modifiers.shift;
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
wayland_client::delegate_noop!(App: ignore wl_region::WlRegion);
wayland_client::delegate_noop!(App: ignore HyprlandGlobalShortcutsManagerV1);
wayland_client::delegate_noop!(App: ignore ZwlrVirtualPointerManagerV1);
wayland_client::delegate_noop!(App: ignore ZwlrVirtualPointerV1);
