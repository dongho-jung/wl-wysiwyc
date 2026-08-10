use crate::atspi::{self, Element};
use crate::config;
use crate::draw::{self, Canvas, Color, Rect};
use crate::grid;
use crate::hint;
use crate::hypr::{self, Snapshot};
use crate::shortcuts::{
    protocol::hyprland_global_shortcut_v1::{self, HyprlandGlobalShortcutV1},
    protocol::hyprland_global_shortcuts_manager_v1::HyprlandGlobalShortcutsManagerV1,
    Key, Shortcuts, Wheel,
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

/// How near the pointer has to get before what it is heading for counts as
/// having caught it, in global logical pixels.
const CAUGHT: f64 = 22.0;

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

/// A run of key presses to put the overlay through, with the pointer's every
/// step written down.
///
/// Navigation is a feel, and a feel cannot be argued about from a distance:
/// what a press does depends on where every label is, how fast a frame takes
/// to paint, and how long a key was down for. This drives the real overlay
/// over the real labels of a real window and says where the pointer went, so
/// that tuning any of it is measuring rather than guessing.
pub struct Drill {
    /// What to press, how long to hold it, in order.
    pub steps: Vec<(Key, Duration)>,
    /// Which window to run it on, 1-based, or the focused one.
    pub win: Option<usize>,
}

impl Drill {
    /// `down:70 wait:400 q:60` and so on: a key and how long it is held, or
    /// a wait with nothing held. Bare names hold for 70ms.
    pub fn parse(script: &str) -> Result<Drill, String> {
        let mut steps = Vec::new();
        for word in script.split([' ', ',']).filter(|w| !w.is_empty()) {
            let (name, ms) = word.split_once(':').unwrap_or((word, "70"));
            let ms: u64 = ms.parse().map_err(|_| format!("{word}: not a duration"))?;
            let key = match name {
                "wait" => Key::Char('\0'),
                "left" => Key::Left,
                "right" => Key::Right,
                "up" => Key::Up,
                "down" => Key::Down,
                "esc" | "escape" => Key::Escape,
                "tab" => Key::Tab,
                "space" => Key::Char(' '),
                "scrollup" => Key::Scroll(Wheel {
                    back: true,
                    far: false,
                    across: false,
                }),
                "scrolldown" => Key::Scroll(Wheel {
                    back: false,
                    far: false,
                    across: false,
                }),
                "click" => Key::LeftClick,
                one if one.chars().count() == 1 => Key::Char(one.chars().next().unwrap()),
                other => return Err(format!("{other}: not a key")),
            };
            steps.push((key, Duration::from_millis(ms)));
        }
        Ok(Drill { steps, win: None })
    }
}

#[derive(Clone, Copy)]
#[allow(clippy::enum_variant_names)]
enum Stage {
    PickWindow,
    PickTile { win: usize },
    PickHint { win: usize },
}

/// The pointer as something with weight, pulled at a target rather than put
/// on one.
///
/// A pointer that is placed where it belongs tells you nothing on the way;
/// one that has to be got moving and then stopped shows you where it went,
/// how far, and which way. Pressing again while it is still moving does not
/// start the journey over, it adds to what is already there, so a run of
/// presses down a list builds speed the way pushing anything repeatedly
/// does.
struct Drift {
    at: (f64, f64),
    vel: (f64, f64),
    /// What is pulling it. A further press moves this without taking away
    /// the speed the pointer already has, so stepping quickly runs the
    /// targets together into one movement rather than a series of darts.
    to: (f64, f64),
    stepped: Instant,
}

impl Drift {
    fn new(at: (f64, f64), to: (f64, f64)) -> Self {
        Drift {
            at,
            vel: (0.0, 0.0),
            to,
            stepped: Instant::now(),
        }
    }

    fn speed(&self) -> f64 {
        self.vel.0.hypot(self.vel.1)
    }

    /// Carry the pointer forward by however long it has been since the last
    /// time, and say whether it has come to rest.
    fn step(&mut self, wall: (f64, f64)) -> bool {
        let now = Instant::now();
        // However long the loop took to come back, the motion is worked out
        // in short steps: taking a long gap in one go would fling the
        // pointer across the screen.
        let mut left = (now - self.stepped).as_secs_f64().min(0.12);
        self.stepped = now;
        let mut done = false;
        while left > 0.0 {
            let dt = left.min(0.008);
            left -= dt;
            done = self.advance(dt, wall);
        }
        done
    }

    /// The step itself, given how long it is. Apart from the clock this is
    /// all there is to the motion, so it is where the numbers can be checked.
    fn advance(&mut self, dt: f64, wall: (f64, f64)) -> bool {
        // A spring, damped just under the point where it would stop dead, so
        // that the pointer settles onto the target rather than halting on it.
        let w = config::get().pointer.spring();
        let (dx, dy) = (self.to.0 - self.at.0, self.to.1 - self.at.1);
        self.vel.0 += (dx * w * w - self.vel.0 * 1.6 * w) * dt;
        self.vel.1 += (dy * w * w - self.vel.1 * 1.6 * w) * dt;
        self.at.0 += self.vel.0 * dt;
        self.at.1 += self.vel.1 * dt;
        // Screens have edges, and a number that runs off one leaves the
        // pointer parked against it with a long way back.
        for (at, vel, edge) in [
            (&mut self.at.0, &mut self.vel.0, wall.0),
            (&mut self.at.1, &mut self.vel.1, wall.1),
        ] {
            if *at < 0.0 || *at > edge {
                *at = at.clamp(0.0, edge);
                *vel = 0.0;
            }
        }
        dx.hypot(dy) < 0.7 && self.speed() < 15.0
    }
}

/// A click key held down. A tap clicks once, and every step of the hold
/// after that asks for one more click; another key while it is down calls
/// the whole thing off.
struct Charge {
    button: u32,
    started: Instant,
}

impl Charge {
    /// How many clicks the hold stands at, and how far along it is to the
    /// next one.
    fn at(&self) -> (u32, f32) {
        config::get().click.stage(self.started.elapsed())
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
    /// Whether this one travels when the window is scrolled.
    scrolls: bool,
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
    /// A click key held down, and since when. How long it is held decides
    /// whether the click is single, double or triple.
    charge: Option<Charge>,
    /// A scroll waiting for the loop to send it, and when to look at the
    /// window again once the scrolling stops.
    scroll: Option<Wheel>,
    settle: Option<Instant>,
    /// Says how far the window moved under the overlay, wheel or otherwise.
    watch: Option<std::sync::mpsc::Receiver<(f64, f64)>>,
    /// With `keys.confirm` on, a key pressed once and not taken yet: it shows
    /// what it would select, and pressing it again takes it.
    armed: Option<char>,
    /// The target a complete hint or tile picked out, waiting for a click
    /// key. What a click key acts on, whether or not the pointer has caught
    /// up with it yet.
    picked: Option<(Target, (f64, f64))>,
    /// What the pointer is nearest at this moment, which is what is drawn
    /// lit. The same thing as `picked` once the pointer has come to rest.
    lit: Option<Target>,
    /// Which arrow keys are down and since when: left, right, up, down. A
    /// held key steps again and again, and `steps` counts how many times it
    /// has, which is what makes it speed up.
    held: [Option<Instant>; 4],
    steps: [u32; 4],
    /// Whether the last frame drawn was dots rather than labels, so that the
    /// labels coming back is a frame and not a wait for one.
    showed_dots: bool,
    /// When the arrows last stepped. While they are being used the labels
    /// give way to a dot on each target: what is being watched is one dot
    /// moving, and a screen of labels is a lot to look through to see it.
    stepping: Option<Instant>,
    elements_cache: HashMap<usize, Vec<Element>>,
    pending_pick: Option<usize>,
    /// Whether nothing has happened since the overlay opened or was last
    /// reset, which is what lets the reset key close it.
    fresh: bool,
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
pub fn run(
    snap: Snapshot,
    smoke: Option<Smoke>,
    drill: Option<Drill>,
) -> Result<Option<(f64, f64)>, Box<dyn Error>> {
    // A 1-based window number from the command line, the focused window when
    // there is none, and the focused window again when the number is out of
    // range.
    let window_or_focused = |n: Option<usize>| match n {
        Some(n) if n >= 1 && n <= snap.windows.len() => n - 1,
        _ => snap.focused,
    };
    let drill_on = drill.as_ref().and_then(|d| d.win);
    let (stage, pending_pick) = match smoke.as_ref().map(|s| &s.view) {
        Some(SmokeView::Picker) => (Stage::PickWindow, None),
        Some(SmokeView::Grid(n)) => (
            Stage::PickTile {
                win: window_or_focused(*n),
            },
            None,
        ),
        Some(SmokeView::Hints(n)) => (Stage::PickWindow, Some(window_or_focused(*n))),
        None => (Stage::PickWindow, Some(window_or_focused(drill_on))),
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
        charge: None,
        scroll: None,
        settle: None,
        watch: None,
        armed: None,
        picked: None,
        lit: None,
        held: [None; 4],
        steps: [0; 4],
        showed_dots: false,
        stepping: None,
        elements_cache: HashMap::new(),
        pending_pick,
        fresh: true,
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
    let shortcuts = match smoke.is_some() || drill.is_some() {
        true => None,
        false => Shortcuts::bind(&globals, &qh),
    };
    layer.set_keyboard_interactivity(
        if smoke.is_some() || drill.is_some() || shortcuts.is_some() {
            KeyboardInteractivity::None
        } else {
            KeyboardInteractivity::Exclusive
        },
    );
    layer.commit();
    app.layer = Some(layer);

    // One virtual pointer for the whole run: it sends the pointer to a picked
    // target while the overlay is up, and clicks once it comes down.
    let pointer = vp_manager.as_ref().map(|mgr| {
        let seat = app.seat_state.seats().next();
        mgr.create_virtual_pointer(seat.as_ref(), &qh, ())
    });

    // Where the pointer is, so a target can be travelled to rather than
    // jumped to, and so a hand on the mouse can be told from the overlay's
    // own doing.
    let mut pointer_at = hypr::cursor_pos();
    let mut drift: Option<Drift> = None;
    // Where the pointer was last seen to be, once it had stopped being moved
    // from here. Compared against itself rather than against where it was
    // last sent, so that the compositor lagging a step behind cannot read as
    // a hand on the mouse.
    let mut resting: Option<(f64, f64)> = None;
    let mut looked = Instant::now();
    let mut painted = Instant::now();

    // A drill presses its own keys on a schedule and says what happened.
    let drilling = drill.is_some();
    let mut script = drill.map(|d| d.steps.into_iter()).into_iter().flatten();
    let mut pending: Option<(Key, Instant)> = None;
    let mut next_step = Instant::now() + Duration::from_millis(400);
    let mut step_no = 0usize;
    let mut settling: Option<Instant> = None;
    // Where the pointer has come to rest, and on what.
    let where_now = |app: &App, at: (f64, f64)| {
        let label = match app.lit {
            Some(Target::Hint(i)) => app.hints.get(i).map(|h| h.label.clone()),
            Some(Target::Tile(c)) => Some(c.to_string()),
            None => None,
        };
        format!(
            "({:.0},{:.0}) on {}",
            at.0,
            at.1,
            label.unwrap_or_else(|| "nothing".into())
        )
    };
    let mut was: Option<((f64, f64), String)> = None;

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
                let wait = if drift.is_some() || app.settle.is_some() {
                    Duration::from_millis(8)
                } else if app.watch.is_some() {
                    // Often enough to catch a scroll that went past the
                    // overlay; the check itself is one message.
                    Duration::from_millis(40)
                } else if app.charging() {
                    // A held key is drawn, not moved, so it wants frames
                    // rather than pointer steps: often enough to look like a
                    // fill, rarely enough that a whole output redraw keeps up.
                    Duration::from_millis(30)
                } else if app.stepping.is_some() {
                    // Waiting for the labels to be due back.
                    Duration::from_millis(60)
                } else if config::get().pointer.cancel_px > 0.0 {
                    // Idle, but still watching the mouse. A hand reaching for
                    // it should not have to wait to be noticed.
                    Duration::from_millis(120)
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
            if drilling {
                let now = Instant::now();
                // However the script goes, a drill ends.
                if start.elapsed() > Duration::from_secs(30) {
                    eprintln!("drill: ran out of time");
                    app.exit = true;
                }
                // Let go of whatever is being held once its time is up.
                if let Some((key, until)) = pending {
                    if now >= until {
                        app.release(key);
                        pending = None;
                        next_step = now + Duration::from_millis(600);
                    }
                }
                // And take the next step when the last one has settled.
                if pending.is_none() && now >= next_step {
                    match script.next() {
                        Some((Key::Char('\0'), how_long)) => {
                            next_step = now + how_long;
                        }
                        Some((key, how_long)) => {
                            step_no += 1;
                            let at = pointer_at.unwrap_or_default();
                            let now_at = where_now(&app, at);
                            match was.take() {
                                Some((before, what)) => eprintln!(
                                    "  landed {now_at}, {:.0}px on{}",
                                    (at.0 - before.0).hypot(at.1 - before.1),
                                    if what == now_at { " (same place)" } else { "" }
                                ),
                                None => eprintln!("  start at {now_at}"),
                            }
                            eprintln!("step {step_no}: {key:?} held {}ms", how_long.as_millis());
                            was = Some((at, now_at));
                            app.press(key);
                            pending = Some((key, now + how_long));
                        }
                        None if settling.is_none() => {
                            settling = Some(now + Duration::from_millis(500));
                        }
                        None => {
                            if settling.is_some_and(|at| now >= at) {
                                let at = pointer_at.unwrap_or_default();
                                let now_at = where_now(&app, at);
                                if let Some((before, what)) = was.take() {
                                    eprintln!(
                                        "  landed {now_at}, {:.0}px on{}",
                                        (at.0 - before.0).hypot(at.1 - before.1),
                                        if what == now_at { " (same place)" } else { "" }
                                    );
                                }
                                app.exit = true;
                            }
                        }
                    }
                }
            }
            if let (Some(wheel), Some(vp)) = (app.scroll.take(), pointer.as_ref()) {
                send_scroll(vp, wheel);
                queue.flush()?;
            }
            // Someone scrolled the window without going through the overlay,
            // which the hints have no way of knowing on their own.
            while let Some(moved) = app.watch.as_ref().and_then(|rx| rx.try_recv().ok()) {
                app.shifted(moved);
            }
            // Everything has moved, so the hints have to be read again. Once
            // the scrolling stops, not on every press of a key held down.
            if app.settle.is_some_and(|at| Instant::now() >= at) {
                app.resettle();
            }
            if app.charging() {
                app.dirty = true;
            }
            // A picked target wants the pointer on it. Doing that here rather
            // than where it was picked keeps the requests and their roundtrip
            // out of the middle of an event callback.
            // A key held down repeats its step, quicker the longer it is
            // held. Holding is meant to cover ground, and covering it target
            // by target keeps every landing on something: a free flight ends
            // wherever it ends, which is what made letting go feel like the
            // pointer was still being pushed.
            app.repeat_held();
            if let Some(to) = app.aim.take() {
                match (drift.as_mut(), pointer_at) {
                    // Already moving: aim it somewhere else and let it keep
                    // the speed it has, so a run of steps is one movement.
                    (Some(d), _) => d.to = to,
                    (None, Some(from)) => drift = Some(Drift::new(from, to)),
                    // Nowhere to travel from, so just be there.
                    (None, None) => {
                        pointer_at = Some(to);
                        if let Some(vp) = pointer.as_ref() {
                            let extent = app.snap.layout_extent;
                            move_and_click(vp, to, extent, None, &mut queue, &mut app)?;
                        }
                        app.light_up(to);
                    }
                }
                resting = None;
            }
            if let (Some(d), Some(vp)) = (drift.as_mut(), pointer.as_ref()) {
                // The pointer is being moved from here, so where it is says
                // nothing about hands.
                looked = Instant::now();
                resting = None;
                let extent = app.snap.layout_extent;
                let wall = ((extent.0 - 1) as f64, (extent.1 - 1) as f64);
                let done = d.step(wall);
                if std::env::var_os("WL_TRACE").is_some() {
                    eprintln!(
                        "t={} at=({:.0},{:.0}) v=({:.0},{:.0}) to=({:.0},{:.0})",
                        start.elapsed().as_millis(),
                        d.at.0,
                        d.at.1,
                        d.vel.0,
                        d.vel.1,
                        d.to.0,
                        d.to.1
                    );
                }
                let caught = (d.to.0 - d.at.0).hypot(d.to.1 - d.at.1) < CAUGHT;
                move_and_click(vp, d.at, extent, None, &mut queue, &mut app)?;
                pointer_at = Some(d.at);
                // What it is heading for lights up when it gets there, not
                // when the key went down: a whole output costs tens of
                // milliseconds to paint, and painting one mid-flight is what
                // would make the flight look stepped.
                if caught || done {
                    app.light_up(d.at);
                }
                if done {
                    drift = None;
                }
            }
            // The labels are due back once the stepping has stopped.
            if app.showed_dots != app.dots() {
                app.dirty = true;
            }
            // A hand on the mouse says the keyboard was not what was wanted
            // after all, so get out of the way. Only a deliberate distance
            // counts, and only while the overlay is not moving the pointer
            // itself, since asking where it is costs a round trip through
            // the compositor.
            let cancel = config::get().pointer.cancel_px;
            if cancel > 0.0 && drift.is_none() && looked.elapsed() > Duration::from_millis(120) {
                looked = Instant::now();
                if let Some(now) = hypr::cursor_pos() {
                    match resting {
                        None => resting = Some(now),
                        Some(was) if (now.0 - was.0).hypot(now.1 - was.1) > cancel => {
                            app.exit = true;
                            continue;
                        }
                        _ => {}
                    }
                }
            }
            if !app.exit && app.configured && app.dirty {
                // A whole output takes long enough to paint that painting one
                // between two steps of a flight is most of the time the
                // flight had to move in. While the pointer is going, the
                // labels keep up as often as they can afford to.
                let spare = drift.is_none() || painted.elapsed() > Duration::from_millis(55);
                if spare {
                    painted = Instant::now();
                    app.draw();
                }
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

/// Turn the wheel over whatever the pointer is on. The overlay takes no
/// pointer input, so this reaches the window underneath the same way a real
/// wheel would.
fn send_scroll(vp: &ZwlrVirtualPointerV1, wheel: Wheel) {
    let axis = match wheel.across {
        true => wl_pointer::Axis::HorizontalScroll,
        false => wl_pointer::Axis::VerticalScroll,
    };
    let way = if wheel.back { -1.0 } else { 1.0 };
    vp.axis_source(wl_pointer::AxisSource::Wheel);
    // A notch is fifteen units by convention, and the discrete count is what
    // an application that counts wheel clicks reads.
    let cfg = &config::get().scroll;
    let mut left = match wheel.far {
        // There is no "scroll to the end" on a wheel, only more of it. One
        // enormous event does not do it: an application is free to clamp what
        // a single event may move, and several do. A run of ordinary ones
        // gets there in every application that scrolls at all.
        true => cfg.far.max(1),
        false => cfg.step.max(1),
    } as i32;
    while left > 0 {
        let n = left.min(10);
        left -= n;
        vp.axis_discrete(0, axis, way * n as f64 * 15.0, (way as i32) * n);
        vp.frame();
        if left > 0 {
            vp.axis_source(wl_pointer::AxisSource::Wheel);
        }
    }
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

    /// The button a key clicks with, if it is a click key at all.
    fn click_button(key: Key) -> Option<u32> {
        match key {
            Key::LeftClick => Some(BTN_LEFT),
            Key::RightClick => Some(BTN_RIGHT),
            _ => None,
        }
    }

    /// Scroll the window under the pointer, and forget the hints: they name
    /// where things were, and where things are is what the scroll changed.
    /// They come back once the scrolling settles, which is the point at which
    /// reading the window again is worth the pause.
    fn roll(&mut self, wheel: Wheel) {
        if matches!(self.stage, Stage::PickWindow) {
            return;
        }
        self.scroll = Some(wheel);
        self.shifted((0.0, 0.0));
    }

    /// The window moved under the overlay: the labels name where things were.
    /// Drop them and read again once it stops moving.
    fn shifted(&mut self, (dx, dy): (f64, f64)) {
        if matches!(self.stage, Stage::PickWindow) {
            return;
        }
        self.settle = Some(Instant::now() + config::get().scroll.settle());
        self.typed.clear();
        self.armed = None;
        self.charge = None;
        self.dirty = true;
        // Carry the labels along rather than leave them behind. Whatever the
        // document moved by, everything in it moved by, so the labels can
        // follow within a frame of the content instead of waiting out the
        // read that will place them exactly.
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        for h in self.hints.iter_mut().filter(|h| h.scrolls) {
            h.rx += dx;
            h.ry += dy;
            h.cx += dx;
            h.cy += dy;
        }
        if let Some((Target::Hint(i), at)) = self.picked {
            if self.hints.get(i).is_some_and(|h| h.scrolls) {
                self.picked = Some((Target::Hint(i), (at.0 + dx, at.1 + dy)));
            }
        }
    }

    /// Read the window again, now that the scrolling has stopped.
    fn resettle(&mut self) {
        let win = match self.stage {
            Stage::PickHint { win } | Stage::PickTile { win } => win,
            Stage::PickWindow => return,
        };
        self.settle = None;
        self.elements_cache.remove(&win);
        self.pending_pick = Some(win);
        self.process_pending_pick();
    }

    /// Whether a hold is running and drawing, which is what the loop watches
    /// to keep the fill moving.
    fn charging(&self) -> bool {
        self.charge.is_some() && config::get().click.charge
    }

    /// The box the fill sits under: whatever a click would land on, in global
    /// logical coordinates.
    fn charge_target(&self) -> Option<(f64, f64, f64, f64)> {
        match (self.picked?, self.stage) {
            ((Target::Hint(i), _), _) => self.hints.get(i).map(|h| (h.rx, h.ry, h.rw, h.rh)),
            ((Target::Tile(ch), _), Stage::PickTile { win }) => {
                let w = &self.snap.windows[win];
                grid::tile_for(w.w as f64, w.h as f64, ch)
                    .map(|t| (w.x as f64 + t.x, w.y as f64 + t.y, t.w, t.h))
            }
            _ => None,
        }
    }

    /// A click key going down starts a hold rather than clicking: how long it
    /// stays down is what says how many clicks are wanted.
    fn hold(&mut self, button: u32) {
        if self.picked.is_none() || self.settle.is_some() {
            return;
        }
        self.charge = Some(Charge {
            button,
            started: Instant::now(),
        });
        self.dirty = true;
    }

    /// A click key coming up spends the hold.
    fn release(&mut self, key: Key) {
        match key {
            Key::Left => return self.hold_arrow(0, false),
            Key::Right => return self.hold_arrow(1, false),
            Key::Up => return self.hold_arrow(2, false),
            Key::Down => return self.hold_arrow(3, false),
            _ => {}
        }
        let (Some(button), Some(charge)) = (Self::click_button(key), self.charge.take()) else {
            return;
        };
        self.dirty = true;
        if charge.button != button {
            return;
        }
        self.spend(charge);
    }

    fn spend(&mut self, charge: Charge) {
        let clicks = config::get().click.clicks(charge.started.elapsed());
        self.click_picked(charge.button, clicks);
    }

    fn press(&mut self, key: Key) {
        match (&self.charge, Self::click_button(key)) {
            // Held keys repeat. A repeat is not a second press, and taking it
            // for one would restart the hold it is meant to be measuring.
            (Some(c), Some(b)) if c.button == b => return,
            // Anything else pressed mid-hold calls the click off, which is
            // how to back out of a hold started by mistake. The key that did
            // it still means what it means.
            (Some(_), _) => {
                self.charge = None;
                self.dirty = true;
            }
            _ => {}
        }
        if let Some(button) = Self::click_button(key) {
            return self.hold(button);
        }
        let ch = match key {
            Key::Escape => return self.cancel(),
            Key::Backspace => return self.undo(),
            Key::Tab => return self.pick_window(),
            Key::Reset => return self.reset_or_quit(),
            Key::Switch => return self.switch_mode(),
            Key::Left => return self.hold_arrow(0, true),
            Key::Right => return self.hold_arrow(1, true),
            Key::Up => return self.hold_arrow(2, true),
            Key::Down => return self.hold_arrow(3, true),
            Key::Scroll(w) => return self.roll(w),
            Key::Char(ch) => ch.to_ascii_lowercase(),
            // Taken above, before anything could cancel a hold.
            Key::LeftClick | Key::RightClick => return,
        };
        if config::get().keys.confirm {
            // Every key is shown before it is taken: the first press arms it,
            // the same key again takes it, and any other key aims elsewhere.
            if self.armed == Some(ch) {
                self.armed = None;
                self.take(ch);
            } else if self.leads_anywhere(ch) {
                self.armed = Some(ch);
                self.fresh = false;
                self.dirty = true;
            }
            return;
        }
        self.take(ch);
    }

    /// Act on a key, now that it is meant.
    fn take(&mut self, ch: char) {
        // The labels on screen name where things were until the window has
        // been read again.
        if self.settle.is_some() {
            return;
        }
        // Typing is asking for the labels, so they come back at once.
        self.stepping = None;
        self.dirty = true;
        self.fresh = false;
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
        //
        // Nothing is redrawn for this. The pointer setting off is the answer
        // to the key, and what it is heading for lights up when it gets
        // there: a whole output is dear enough to paint that doing it while
        // the pointer is moving is what would make the move look stepped.
        self.aim = Some(at);
    }

    /// Every target the current mode offers, with where clicking it would
    /// land.
    fn targets(&self) -> Vec<(Target, (f64, f64))> {
        self.boxed().into_iter().map(|(t, at, _)| (t, at)).collect()
    }

    /// Every target with the box it stands for, which is what stepping goes
    /// by: a list of links is a column to the eye however much their middles
    /// wander with the length of the text.
    fn boxed(&self) -> Vec<(Target, (f64, f64), Rect)> {
        match self.stage {
            Stage::PickHint { .. } => self
                .hints
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    (
                        Target::Hint(i),
                        (h.cx, h.cy),
                        Rect::new(h.rx as f32, h.ry as f32, h.rw as f32, h.rh as f32),
                    )
                })
                .collect(),
            Stage::PickTile { win } => {
                let w = &self.snap.windows[win];
                grid::tiles(w.w as f64, w.h as f64)
                    .into_iter()
                    .map(|t| {
                        let (x, y) = (w.x as f64 + t.x, w.y as f64 + t.y);
                        (
                            Target::Tile(t.ch),
                            (x + t.w / 2.0, y + t.h / 2.0),
                            Rect::new(x as f32, y as f32, t.w as f32, t.h as f32),
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
    /// An arrow key going down or coming up. Holding one pushes the pointer
    /// that way for as long as it is held, so a tap nudges it to the next
    /// thing along and a hold carries it across the window.
    fn hold_arrow(&mut self, which: usize, down: bool) {
        let already = self.held[which].is_some();
        // Always restamp on the way down. A press that finds the key already
        // held is a release that went missing, and leaving the old stamp
        // there would leave that direction dead.
        self.held[which] = down.then(Instant::now);
        self.steps[which] = 0;
        if already == down || !down {
            return;
        }
        self.fresh = false;
        self.dirty |= !self.typed.is_empty() || self.armed.is_some();
        self.typed.clear();
        self.armed = None;
        self.charge = None;
        self.hop(Self::WAYS[which], false);
    }

    /// The four directions, in the order the keys are held in.
    const WAYS: [(f64, f64); 4] = [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)];

    /// Whether the labels are out of the way at this moment, which they are
    /// while the arrows are stepping and for a moment after.
    fn dots(&self) -> bool {
        self.stepping
            .is_some_and(|t| t.elapsed() < config::get().label.wake())
    }

    /// Send the pointer to the next target that way.
    ///
    /// A press will cross a gap and, going up or down, carry on in reading
    /// order when nothing is in line. A key held down will not do either: it
    /// keeps to the run it is walking and stops at the end of it. Otherwise
    /// holding down a list runs off the end of it, leaps to whatever section
    /// is next, and keeps going there, which is a key that has taken on a
    /// life of its own.
    fn hop(&mut self, way: (f64, f64), held: bool) -> bool {
        self.stepping = Some(Instant::now());
        let here = self.picked.map(|(p, _)| p);
        let from = match self.picked {
            Some((_, at)) => at,
            None => match hypr::cursor_pos() {
                Some(at) => at,
                None => return false,
            },
        };
        let all = self.boxed();
        let mine = all
            .iter()
            .find(|(what, _, _)| here == Some(*what))
            .map(|(_, _, r)| *r)
            // Nothing picked yet, so the pointer itself is the box to go from.
            .unwrap_or(Rect::new(from.0 as f32, from.1 as f32, 1.0, 1.0));
        let targets: Vec<(Target, (f64, f64), Rect)> = all
            .into_iter()
            .filter(|&(what, _, _)| here != Some(what))
            .collect();
        let boxes: Vec<Rect> = targets.iter().map(|&(_, _, r)| r).collect();
        let reach = config::get().pointer.reach_px;
        let reach = if held { reach / 3.0 } else { reach };
        match pull(mine, way, &boxes, reach, !held) {
            Some(i) => {
                let (what, at, _) = targets[i];
                if std::env::var_os("WL_KEYS").is_some() {
                    eprintln!("  hop {:?} -> ({:.0},{:.0})", way, at.0, at.1);
                }
                self.focus(what, at);
                true
            }
            None => {
                if std::env::var_os("WL_KEYS").is_some() {
                    eprintln!("  hop {way:?} -> nothing");
                }
                false
            }
        }
    }

    /// Step again for any key still held down, quicker the longer it has
    /// been held.
    ///
    /// A held key is asking to cover ground, and covering it a target at a
    /// time keeps every landing on something. The alternative, letting the
    /// pointer off its leash while the key is down, ends the journey wherever
    /// the pointer happens to be when the key comes up, which is what made
    /// letting go feel like the key was still down.
    fn repeat_held(&mut self) {
        for i in 0..4 {
            let Some(since) = self.held[i] else { continue };
            // A key held longer than anyone holds a key has not been let go
            // of: the release was lost, and the pointer should stop.
            if since.elapsed() > Duration::from_millis(4000) {
                self.held[i] = None;
                continue;
            }
            let cfg = &config::get().pointer;
            let due = match self.steps[i] {
                0 => cfg.repeat(),
                n => cfg.again(n),
            };
            if since.elapsed() >= due {
                self.steps[i] += 1;
                self.held[i] = Some(Instant::now());
                // The end of the run the key was walking: stop there rather
                // than set off across the window.
                if !self.hop(Self::WAYS[i], true) {
                    self.held[i] = None;
                }
            }
        }
    }

    /// Whatever the pointer is nearest, and how far off it is.
    fn nearest(&self, at: (f64, f64)) -> Option<(Target, (f64, f64), f64)> {
        self.targets()
            .into_iter()
            .map(|(what, p)| (what, p, (p.0 - at.0).hypot(p.1 - at.1)))
            .min_by(|a, b| a.2.total_cmp(&b.2))
    }

    /// Light up whatever the pointer is nearest now, and while it is being
    /// flown, pick it too: there is no target waiting at the end of the trip,
    /// so what it is over is what a click would take.
    fn light_up(&mut self, at: (f64, f64)) {
        let Some((what, _, _)) = self.nearest(at) else {
            return;
        };
        if Some(what) != self.lit {
            self.lit = Some(what);
            self.dirty = true;
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
    /// then everything typed, then quit. Nothing having happened yet counts
    /// as unwound already, so opening the overlay and changing your mind is
    /// one press rather than two.
    fn cancel(&mut self) {
        // What the pointer is over is not a state to back out of: it is
        // wherever the pointer is, and it will be over something else the
        // moment the pointer moves. So Esc unwinds what was typed, and
        // leaves.
        if !self.fresh && (self.armed.take().is_some() || !self.typed.is_empty()) {
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
            self.fresh = false;
            self.dirty = true;
        }
    }

    fn reset_input(&mut self) {
        self.typed.clear();
        self.armed = None;
        self.picked = None;
        // Hints are about to be rebuilt in some of the ways here, and what
        // was lit was an index into the old ones.
        self.lit = None;
        self.fresh = true;
        self.dirty = true;
    }

    /// The reset key puts the overlay back to how it opened, and closes it
    /// when it is already there. Bound to whatever key opens the overlay,
    /// that reads as one key doing one thing: press it again to undo a wrong
    /// turn, press it once more to leave.
    fn reset_or_quit(&mut self) {
        if self.fresh {
            self.exit = true;
            return;
        }
        self.reset_input();
        self.focus_nearest_cursor();
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
        // Watch something inside the document, since that is the part that
        // moves, and the one nearest the middle of the window, since that is
        // the part least likely to be pinned to an edge.
        let w = &self.snap.windows[win];
        let (mx, my) = (w.w as f64 / 2.0, w.h as f64 / 2.0);
        self.watch = els
            .iter()
            .filter(|e| e.scrolls)
            .min_by(|a, b| {
                let d = |e: &Element| (e.x - mx).powi(2) + (e.y - my).powi(2);
                d(a).total_cmp(&d(b))
            })
            .and_then(atspi::watch_bounds);
        self.hints = hints_for(w, &els);
        self.stage = Stage::PickHint { win };
        self.reset_input();
    }

    fn draw(&mut self) {
        self.dirty = false;
        let dots = self.dots();
        self.showed_dots = dots;
        // Worked out before the buffer is borrowed, since it reads the hints
        // and the snapshot that the borrow would tie up.
        let charge = self
            .charge
            .as_ref()
            .filter(|_| self.charging())
            .and_then(|c| {
                let (ex, ey, ew, eh) = self.charge_target()?;
                let mon = &self.snap.monitor;
                let s = self.buffer_scale as f64;
                let rect = Rect::new(
                    ((ex - mon.x as f64) * s) as f32,
                    ((ey - mon.y as f64) * s) as f32,
                    (ew * s) as f32,
                    (eh * s) as f32,
                );
                Some((rect, c.at()))
            });
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
                let lit = match self.lit {
                    Some(Target::Tile(ch)) => Some(ch),
                    _ => self.armed,
                };
                draw_pick_tile(&self.snap, win, lit, &self.font, &mut canvas, scale)
            }
            Stage::PickHint { win } => {
                let view = HintView {
                    hints: &self.hints,
                    typed: &self.typed,
                    armed: self.armed,
                    picked: match self.lit {
                        Some(Target::Hint(i)) => Some(i),
                        _ => None,
                    },
                    stale: self.settle.is_some(),
                    dots,
                };
                draw_pick_hint(&self.snap, win, view, &self.font, &mut canvas, scale)
            }
        }
        if let Some((rect, at)) = charge {
            draw_charge(&mut canvas, rect, at, &self.font, scale as f32);
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

/// Which of these points a step in a direction should land on.
///
/// Whichever pulls hardest, which is mostly whichever is nearest: a step is
/// meant to land on the next thing over, and ranking by how well something
/// lines up is what reaches past three of them for a fourth. Being off the
/// line counts against a target too, enough to keep a row or a column
/// reading straight when the choice is otherwise close.
///
/// Nothing further off than `reach` counts at all. Every layout has edges,
/// and at one of them the next thing that way can be the far side of the
/// window: answering a press meaning "the next one down" with a jump across
/// the screen is worse than answering it with nothing, and holding the key
/// flies there under your own hand anyway.
fn pull(
    from: Rect,
    (dx, dy): (f64, f64),
    boxes: &[Rect],
    reach: f64,
    wander: bool,
) -> Option<usize> {
    /// What a pixel to the side costs, against a pixel of the way there.
    const ACROSS: f64 = 2.0;
    /// How far to the side is still that way at all: a fixed allowance for
    /// near neighbours, and a share of the distance for far ones.
    const SPREAD: (f64, f64) = (40.0, 0.3);

    // How far apart two spans are, and nothing if they overlap: two boxes
    // that share any of the same rows are on the same line as far as a press
    // to the side is concerned, however wide either of them is.
    let gap = |(a0, a1): (f64, f64), (b0, b1): (f64, f64)| (b0 - a1).max(a0 - b1).max(0.0);
    let span = |r: &Rect, across: bool| match across {
        // The axis the press is not along.
        true if dx != 0.0 => (r.y as f64, (r.y + r.h) as f64),
        true => (r.x as f64, (r.x + r.w) as f64),
        false if dx != 0.0 => (r.x as f64, (r.x + r.w) as f64),
        false => (r.y as f64, (r.y + r.h) as f64),
    };
    // That way means starting further that way, by the edge nearest the
    // press and not by the middle. A label and the field beside it are a row
    // apart in their middles and a hand's width apart in their edges: going
    // by middles, the label below wins a press of right, which is how right
    // ends up meaning down.
    let edge = |r: &Rect| match (dx > 0.0, dy > 0.0, dx != 0.0) {
        (true, _, true) => r.x as f64,
        (false, _, true) => -((r.x + r.w) as f64),
        (_, true, false) => r.y as f64,
        (_, false, false) => -((r.y + r.h) as f64),
    };
    let ahead = |r: &Rect| edge(r) >= edge(&from) + 4.0;

    let best = boxes
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let along = gap(span(&from, false), span(r, false));
            let across = gap(span(&from, true), span(r, true));
            let allowed = SPREAD.0 + SPREAD.1 * along;
            (ahead(r) && across <= allowed && along <= reach)
                .then_some((along + ACROSS * across, i))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, i)| i);
    if best.is_some() || dx != 0.0 || !wander {
        return best;
    }

    // Nothing below, or above, that is also in line. In a paragraph that is
    // the usual case rather than the odd one: the links are wherever the
    // words fell, so the one on the next line is rarely under the one on
    // this line, and a rule about being in line has down doing nothing at
    // all. What down means there is the next one along, so take that: the
    // first thing on a later line, reading the way the text does.
    let (top, bottom) = (from.y as f64, (from.y + from.h) as f64);
    boxes
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            let (rt, rb) = (r.y as f64, (r.y + r.h) as f64);
            match dy > 0.0 {
                true => rt >= bottom - 1.0 && rt - bottom <= reach,
                false => rb <= top + 1.0 && top - rb <= reach,
            }
        })
        .min_by(|(_, a), (_, b)| {
            let key = |r: &Rect| match dy > 0.0 {
                true => (r.y as f64, r.x as f64),
                // Reading backwards: the last thing on the line above.
                false => (-(r.y + r.h) as f64, -(r.x + r.w) as f64),
            };
            let (a, b) = (key(a), key(b));
            a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1))
        })
        .map(|(i, _)| i)
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
            config::get().keys.layout,
        ))
        .map(|((e, &(cx, cy)), label)| Hint {
            label,
            scrolls: e.scrolls,
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
    // "dw:0.6" is that label picked out with a click key held six tenths of
    // the way through, which is the only way to look at the fill.
    let (keys, held) = match keys.split_once(':') {
        Some((k, f)) => (k, f.parse::<f32>().ok()),
        None => (keys, None),
    };
    let els = atspi::clickable_elements(w.pid, &w.title).unwrap_or_else(|e| {
        eprintln!("atspi: {e}");
        Vec::new()
    });
    // Trailing "." means every key confirmed and none armed, the state right
    // after a confirming press.
    let mut typed = keys.to_string();
    let mut armed = match typed.strip_suffix('.') {
        Some(rest) => {
            typed = rest.to_string();
            None
        }
        None => typed.pop(),
    };
    let charge = |canvas: &mut Canvas, el: Rect| {
        if let Some(t) = held {
            let cfg = &config::get().click;
            let stage = cfg.stage(cfg.span().mul_f32(t.clamp(0.0, 1.0)));
            draw_charge(canvas, el, stage, &font, scale as f32);
        }
    };
    if els.is_empty() {
        draw_pick_tile(snap, win, armed, &font, &mut canvas, scale);
        // A whole tile letter is a tile picked out, the same as a whole label.
        if let Some(t) = keys.chars().next().filter(|_| keys.chars().count() == 1) {
            if let Some(tile) = grid::tile_for(w.w as f64, w.h as f64, t) {
                let mon = &snap.monitor;
                let el = Rect::new(
                    ((w.x - mon.x) as f64 + tile.x) as f32 * scale as f32,
                    ((w.y - mon.y) as f64 + tile.y) as f32 * scale as f32,
                    tile.w as f32 * scale as f32,
                    tile.h as f32 * scale as f32,
                );
                charge(&mut canvas, el);
            }
        }
        return Ok((buf, bw, bh));
    }
    let hints = hints_for(w, &els);
    // A label typed out in full is one that has been picked, not one still
    // being narrowed down.
    let picked = match keys {
        "dots" => Some(hints.len() / 2),
        _ => hints.iter().position(|h| h.label == keys),
    };
    if picked.is_some() {
        typed = keys.to_string();
        armed = None;
    }
    let view = HintView {
        hints: &hints,
        typed: &typed,
        armed,
        picked,
        stale: false,
        // "dots" asks for the view the arrow keys leave behind.
        dots: keys == "dots",
    };
    draw_pick_hint(snap, win, view, &font, &mut canvas, scale);
    if let Some(i) = picked {
        let h = &hints[i];
        let mon = &snap.monitor;
        charge(
            &mut canvas,
            Rect::new(
                ((h.rx - mon.x as f64) * scale as f64) as f32,
                ((h.ry - mon.y as f64) * scale as f64) as f32,
                (h.rw * scale as f64) as f32,
                (h.rh * scale as f64) as f32,
            ),
        );
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
        // Nothing clear anywhere: take whichever spot hides the least of the
        // labels already down. The first spot was the old answer, and it can
        // land squarely on a neighbour, which reads as a target with no hint
        // at all rather than as two labels sharing a corner.
        let fallback = || {
            spots
                .iter()
                .map(|&s| fit(s))
                .min_by_key(|b| {
                    let over: i64 = taken.iter().map(|t| b.covers((t.x, t.y, t.w, t.h))).sum();
                    let hidden: i64 = elements.iter().map(|&e| b.covers(e)).sum();
                    (over, hidden)
                })
                .unwrap_or_else(|| fit(spots[0]))
        };
        taken.push(best.map(|(_, b)| b).unwrap_or_else(fallback));
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
    /// The window moved and these labels have not caught up. They are still
    /// drawn, faded, rather than taken away: a label that goes out and comes
    /// back is two flinches where fading is none, and the fade says by itself
    /// that they are not to be typed yet.
    stale: bool,
    /// The arrows are stepping, so the labels give way to a dot apiece.
    dots: bool,
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
        stale,
        dots,
    } = view;
    // How much of itself a label is showing. Stale ones step back.
    let lit = |c: Color| if stale { c.fade(0.55) } else { c };
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
    // Stepping with the arrows: a dot on each target and nothing else, so
    // that the one being moved between them is the thing on screen.
    if dots {
        for (i, h) in hints.iter().enumerate() {
            let hot = picked == Some(i);
            let d = (if hot { 10.0 } else { 6.0 }) * s;
            let spot = Rect::new(
                ((h.cx - mon.x as f64) * scale as f64) as f32 - d / 2.0,
                ((h.cy - mon.y as f64) * scale as f64) as f32 - d / 2.0,
                d,
                d,
            );
            canvas.round_rect_shadow(
                spot,
                d / 2.0,
                3.0 * s,
                shade(*config::get().colors.shadow, 0.8),
            );
            let colour = match hot {
                true => *config::get().colors.ring,
                false => *config::get().colors.dot,
            };
            canvas.round_rect(spot, d / 2.0, colour);
            if hot {
                canvas.round_rect_shadow(spot.grow(2.0 * s), d, 7.0 * s, colour.fade(0.5));
            }
        }
        return;
    }
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
            // The colours a chip is kept under, before the fade that a stale
            // label wears: fading here as well would fade it twice.
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
                canvas.round_rect_shadow(el, 4.0 * s, 6.0 * s, lit(armed_glow().fade(0.45)));
                canvas.round_rect_outline(el, 4.0 * s, 2.0 * s, lit(ring));
            }
            let r = b.rect();
            let radius = r.h * 0.3;
            if hot && clinches {
                // One label at a time wears the glow, so it is drawn the long
                // way round rather than kept.
                canvas.round_rect_shadow(r, radius, 7.0 * s, lit(armed_glow()));
                canvas.round_rect(r, radius, lit(bg));
                canvas.round_rect_outline(r, radius, s, lit(edge));
            } else {
                // Every other label is the same picture in a different place.
                canvas.chip(
                    r,
                    draw::Style {
                        radius,
                        blur: 2.6 * s,
                        drop: 1.2 * s,
                        bg,
                        edge,
                        edge_w: s,
                        shadow: *config::get().colors.shadow,
                    },
                    if stale { 0.55 } else { 1.0 },
                );
            }
            let cap = hot && armed.is_some();
            draw_label_text(
                canvas,
                font,
                r,
                &h.label,
                typed.len(),
                cap,
                px,
                lit(text),
                s,
            );
        }
    }
}

/// The charge on a held click key: what it has stored up, and what is coming.
///
/// A ring gathers in on the target as the step fills and lands on it when the
/// count goes up, a wave breaks outward at the moment it does, the target's
/// own outline thickens and glows with what is stored, and a badge says the
/// count outright. The badge is the part that answers the only question that
/// matters while holding: let go now, and how many clicks is that?
fn draw_charge(canvas: &mut Canvas, el: Rect, (level, frac): (u32, f32), font: &Font, s: f32) {
    let cfg = config::get();
    let tone = |n: u32| match n {
        0 | 1 => *cfg.colors.charge,
        2 => *cfg.colors.armed,
        _ => *cfg.colors.ring,
    };
    let ink = |n: u32| match n {
        0 | 1 => *cfg.colors.hint_text,
        _ => *cfg.colors.armed_text,
    };
    let radius = 5.0 * s;
    let here = tone(level);
    let top = cfg.click.levels();

    // What is stored: the target sits inside a ring that grows with it.
    canvas.round_rect_shadow(el, radius, (9.0 + 6.0 * level as f32) * s, here.fade(0.42));
    canvas.round_rect_outline(el, radius, (1.6 + 1.1 * level as f32) * s, here);
    // Nothing more is coming, and the target says so by wearing two more.
    if level >= top {
        for (pad, alpha) in [(5.0, 0.7), (10.0, 0.3)] {
            let pad = pad * s;
            canvas.round_rect_outline(el.grow(pad), radius + pad, 1.6 * s, here.fade(alpha));
        }
    }

    // What is coming: a ring closing in, quicker the nearer it gets, so the
    // moment it lands is the moment the count goes up.
    if level < top {
        let ease = frac * frac;
        let pad = (22.0 - 18.0 * ease) * s;
        canvas.round_rect_outline(
            el.grow(pad),
            radius + pad,
            (1.2 + 2.6 * ease) * s,
            tone(level + 1).fade(0.12 + 0.85 * ease),
        );
    }

    // And the wave off the one that just landed, which is what makes a step
    // something you feel rather than something you notice afterwards.
    if level > 1 && frac < 0.4 {
        let k = frac / 0.4;
        for (lag, weight) in [(0.0, 1.0), (0.25, 0.45)] {
            let k = ((k - lag) / (1.0 - lag)).clamp(0.0, 1.0);
            let pad = (3.0 + 46.0 * k) * s;
            canvas.round_rect_outline(
                el.grow(pad),
                radius + pad,
                (3.4 - 2.2 * k) * s,
                here.fade((1.0 - k) * (1.0 - k) * 0.9 * weight),
            );
        }
    }

    // The count, in as many words. A badge over the target, or under it when
    // the target sits against the top of the screen.
    let px = cfg.label.size * (1.25 + 0.12 * level as f32) * s;
    let text = format!("x{level}");
    let (pad_x, pad_y) = (cfg.label.pad_x * s, cfg.label.pad_y * s);
    let bw = draw::text_width(font, &text, px) + 2.0 * pad_x;
    let bh = px + 2.0 * pad_y;
    let above = el.y - bh - 7.0 * s;
    let badge = Rect::new(
        el.x + (el.w - bw) / 2.0,
        if above > 0.0 {
            above
        } else {
            (el.y + el.h + 7.0 * s).min(canvas.h as f32 - bh)
        },
        bw,
        bh,
    );
    canvas.round_rect_shadow(
        badge.shift(0.0, 1.5 * s),
        bh * 0.35,
        5.0 * s,
        shade(*cfg.colors.shadow, 0.75),
    );
    canvas.round_rect(badge, bh * 0.35, here);
    canvas.text_centered(
        font,
        &text,
        badge.x + bw / 2.0,
        badge.y + bh / 2.0,
        px,
        ink(level),
    );
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
    if cfg.left().contains(&name) {
        return Some(Key::LeftClick);
    }
    if cfg.right().contains(&name) {
        return Some(Key::RightClick);
    }
    if cfg.reset().is_some_and(|k| k == name) {
        return Some(Key::Reset);
    }
    if cfg.switch().is_some_and(|k| k == name) {
        return Some(Key::Switch);
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
        match event {
            hyprland_global_shortcut_v1::Event::Pressed { .. } => {
                if std::env::var_os("WL_KEYS").is_some() {
                    eprintln!("KEY down {}", key.name());
                }
                state.press(*key)
            }
            hyprland_global_shortcut_v1::Event::Released { .. } => {
                if std::env::var_os("WL_KEYS").is_some() {
                    eprintln!("KEY up   {}", key.name());
                }
                state.release(*key)
            }
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
            self.press(key);
        }
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if let Some(key) = key_of(&event) {
            self.release(key);
        }
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
wayland_client::delegate_noop!(App: ignore wl_region::WlRegion);
wayland_client::delegate_noop!(App: ignore HyprlandGlobalShortcutsManagerV1);
wayland_client::delegate_noop!(App: ignore ZwlrVirtualPointerManagerV1);
wayland_client::delegate_noop!(App: ignore ZwlrVirtualPointerV1);

#[cfg(test)]
mod tests {
    use super::{pull, Drift, Rect};

    const RIGHTWARD: (f64, f64) = (1.0, 0.0);
    const DOWNWARD: (f64, f64) = (0.0, 1.0);
    const REACH: f64 = 500.0;
    /// Somewhere for the pointer to run out of, well past anywhere these
    /// tests send it.
    const WALL: (f64, f64) = (10_000.0, 10_000.0);

    fn at(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::new(x, y, w, h)
    }

    /// Step to a target and watch the pointer get there.
    fn travel(from: (f64, f64), to: (f64, f64)) -> (u32, f64) {
        let mut d = Drift::new(from, to);
        let mut ticks = 0;
        let mut furthest: f64 = 0.0;
        for _ in 0..400 {
            ticks += 1;
            let done = d.advance(0.008, WALL);
            furthest = furthest.max(d.at.0 - to.0);
            if done {
                break;
            }
        }
        (ticks, furthest)
    }

    #[test]
    fn a_list_of_links_is_a_column_however_long_the_links_are() {
        // What a real page looks like: rows eighteen apart, left aligned,
        // and so wide apart in the middle that going by middles has them in
        // different columns and stepping down stops working altogether.
        let rows: Vec<Rect> = (0..6)
            .map(|i| {
                at(
                    645.0,
                    476.0 + i as f32 * 18.0,
                    120.0 + i as f32 * 60.0,
                    16.0,
                )
            })
            .collect();
        for i in 0..5 {
            let rest: Vec<Rect> = rows[i + 1..].to_vec();
            let next = pull(rows[i], DOWNWARD, &rest, REACH, true);
            assert_eq!(next, Some(0), "row {i} did not step to the one below");
        }
    }

    #[test]
    fn a_column_over_is_still_that_way() {
        // Two columns of a page, four hundred pixels apart: far, but level
        // and plainly to the right.
        let left = at(215.0, 476.0, 145.0, 16.0);
        let right = [at(645.0, 476.0, 119.0, 16.0), at(645.0, 800.0, 119.0, 16.0)];
        assert_eq!(pull(left, RIGHTWARD, &right, REACH, true), Some(0));
    }

    #[test]
    fn nothing_that_way_is_better_than_something_the_other_way() {
        let from = at(600.0, 500.0, 80.0, 16.0);
        // Up and to the right, at sixty degrees: not what right means.
        let corner = [at(700.0, 300.0, 80.0, 16.0)];
        assert_eq!(pull(from, RIGHTWARD, &corner, REACH, true), None);
        // And nothing at all beyond arm's length.
        let far = [at(1400.0, 500.0, 80.0, 16.0)];
        assert_eq!(pull(from, RIGHTWARD, &far, 500.0, true), None);
        assert_eq!(pull(from, RIGHTWARD, &far, 900.0, true), Some(0));
    }

    #[test]
    fn the_nearest_one_that_way_wins() {
        let from = at(0.0, 0.0, 40.0, 20.0);
        let boxes = [
            at(300.0, 0.0, 40.0, 20.0),
            at(80.0, 0.0, 40.0, 20.0),
            at(80.0, 44.0, 40.0, 20.0),
        ];
        assert_eq!(pull(from, RIGHTWARD, &boxes, REACH, true), Some(1));
        assert_eq!(pull(from, DOWNWARD, &boxes, REACH, true), Some(2));
    }

    #[test]
    fn a_step_arrives_and_settles() {
        let (ticks, past) = travel((0.0, 0.0), (300.0, 0.0));
        let ms = ticks * 8;
        assert!((80..400).contains(&ms), "a step took {ms}ms");
        // A little past the target and back is the point; a long way past it
        // is a pointer that has to be chased.
        assert!(past < 20.0, "overshot by {past:.0}px");
    }

    #[test]
    fn a_step_stops_at_the_edge_of_the_screen() {
        let mut d = Drift::new((0.0, 0.0), (9000.0, 0.0));
        for _ in 0..200 {
            d.advance(0.008, (300.0, 300.0));
        }
        assert_eq!(d.at.0, 300.0, "went past the edge");
        assert_eq!(d.vel.0, 0.0, "still carrying speed into the wall");
    }
}
