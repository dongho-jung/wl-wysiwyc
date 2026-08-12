use crate::atspi::{self, Element};
use crate::config;
use crate::draw::{self, Canvas, Color, Rect};
use crate::grid;
use crate::hint;
use crate::hypr::{self, Snapshot};
use crate::shortcuts::{
    protocol::hyprland_global_shortcut_v1::{self, HyprlandGlobalShortcutV1},
    protocol::hyprland_global_shortcuts_manager_v1::HyprlandGlobalShortcutsManagerV1,
    Arrow, Key, NavMode, Shortcuts, Wheel,
};
use fontdue::Font;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_registry,
    delegate_seat, delegate_shm, delegate_subcompositor,
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
    shm::{
        slot::{Buffer, SlotPool},
        Shm, ShmHandler,
    },
    subcompositor::SubcompositorState,
};
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::io::ErrorKind;
use std::time::{Duration, Instant};
use wayland_client::{
    backend::WaylandError,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_keyboard, wl_output, wl_pointer, wl_region, wl_registry, wl_seat, wl_shm, wl_subsurface,
        wl_surface,
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
const POINTER_SURFACE_SIZE: i32 = 48;
const ANCHOR_SURFACE_SIZE: i32 = 32;
const MOTION_FRAME: Duration = Duration::from_millis(8);
const EDGE_SCROLL_INTERVAL: Duration = Duration::from_millis(90);
const EDGE_INSET: f64 = 3.0;
const INITIAL_THRUST: f64 = 0.32;

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
    /// What to press together, how long to hold the chord, in order.
    pub steps: Vec<(Vec<Key>, Duration)>,
    /// Which window to run it on, 1-based, or the focused one.
    pub win: Option<usize>,
}

impl Drill {
    /// `down:70 down+right:250 wait:400` and so on: a chord and how long it
    /// is held, or a wait with nothing held. Bare names hold for 70ms.
    pub fn parse(script: &str) -> Result<Drill, String> {
        let key = |name: &str| match name {
            "left" => Ok(Key::Arrow(Arrow::Left, NavMode::Normal)),
            "right" => Ok(Key::Arrow(Arrow::Right, NavMode::Normal)),
            "up" => Ok(Key::Arrow(Arrow::Up, NavMode::Normal)),
            "down" => Ok(Key::Arrow(Arrow::Down, NavMode::Normal)),
            "scroll-left" | "scrollleft" | "shift-left" => {
                Ok(Key::ShiftScroll(Arrow::Left.wheel()))
            }
            "scroll-right" | "scrollright" | "shift-right" => {
                Ok(Key::ShiftScroll(Arrow::Right.wheel()))
            }
            "shift-up" => Ok(Key::ShiftScroll(Arrow::Up.wheel())),
            "shift-down" => Ok(Key::ShiftScroll(Arrow::Down.wheel())),
            "scroll-up" | "scrollup" => Ok(Key::Scroll(Arrow::Up.wheel())),
            "scroll-down" | "scrolldown" => Ok(Key::Scroll(Arrow::Down.wheel())),
            "free-left" | "alt-left" => Ok(Key::Arrow(Arrow::Left, NavMode::Free)),
            "free-right" | "alt-right" => Ok(Key::Arrow(Arrow::Right, NavMode::Free)),
            "free-up" | "alt-up" => Ok(Key::Arrow(Arrow::Up, NavMode::Free)),
            "free-down" | "alt-down" => Ok(Key::Arrow(Arrow::Down, NavMode::Free)),
            "instant-left" | "ctrl-left" => Ok(Key::Arrow(Arrow::Left, NavMode::Instant)),
            "instant-right" | "ctrl-right" => Ok(Key::Arrow(Arrow::Right, NavMode::Instant)),
            "instant-up" | "ctrl-up" => Ok(Key::Arrow(Arrow::Up, NavMode::Instant)),
            "instant-down" | "ctrl-down" => Ok(Key::Arrow(Arrow::Down, NavMode::Instant)),
            "esc" | "escape" => Ok(Key::Escape),
            "tab" => Ok(Key::Tab),
            "space" => Ok(Key::Char(' ')),
            "click" => Ok(Key::LeftClick),
            one if one.chars().count() == 1 => Ok(Key::Char(one.chars().next().unwrap())),
            other => Err(format!("{other}: not a key")),
        };
        let mut steps = Vec::new();
        for word in script.split([' ', ',']).filter(|w| !w.is_empty()) {
            let (name, ms) = word.split_once(':').unwrap_or((word, "70"));
            let ms: u64 = ms.parse().map_err(|_| format!("{word}: not a duration"))?;
            let chord = match name {
                "wait" => Vec::new(),
                _ => name.split('+').map(&key).collect::<Result<Vec<_>, _>>()?,
            };
            steps.push((chord, Duration::from_millis(ms)));
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum HintDisplay {
    #[default]
    Labels,
    ScrollAnchors,
}

impl HintDisplay {
    fn scrolled(&mut self) {
        *self = Self::ScrollAnchors;
    }

    fn typed(&mut self, ch: char) {
        if ch.is_ascii_alphabetic() {
            *self = Self::Labels;
        }
    }

    fn shows_scroll_anchors(self) -> bool {
        self == Self::ScrollAnchors
    }
}

/// A short-lived filter that keeps a new directional gesture from choosing
/// its source or an anchor behind the requested direction.
#[derive(Clone, Copy, Debug, PartialEq)]
struct DepartureGuard {
    anchor: (f64, f64),
    direction: (f64, f64),
}

impl DepartureGuard {
    fn new(anchor: (f64, f64), direction: (f64, f64)) -> Self {
        let mut guard = Self {
            anchor,
            direction: (0.0, 0.0),
        };
        guard.aim(direction);
        guard
    }

    fn aim(&mut self, direction: (f64, f64)) {
        let length = direction.0.hypot(direction.1);
        if length > f64::EPSILON {
            self.direction = (direction.0 / length, direction.1 / length);
        }
    }

    fn excludes(self, point: (f64, f64)) -> bool {
        let offset = (point.0 - self.anchor.0, point.1 - self.anchor.1);
        let source = offset.0.hypot(offset.1) <= 0.5;
        // A half-pixel tolerance keeps a nearly perpendicular row or column
        // eligible despite small coordinate differences.
        let behind = offset.0 * self.direction.0 + offset.1 * self.direction.1 < -0.5;
        source || behind
    }
}

/// The pointer as something with weight, pulled at a target rather than put
/// on one.
///
/// Arrow input accelerates it directly, release leaves a bounded coast, and
/// the selected anchor keeps pulling until a close pass can snap. Named hints
/// use the same spring without resetting existing velocity.
struct Drift {
    at: (f64, f64),
    vel: (f64, f64),
    /// What has caught it. None means the arrow keys are steering it freely.
    to: Option<(f64, f64)>,
    /// The anchor pulling on released free motion. This is separate from
    /// `to`: attraction changes velocity continuously, while `to` is the
    /// close-range spring that commits the landing to an exact point.
    magnet: Option<(f64, f64)>,
    /// The anchor this input departed and the direction it requested. The
    /// source and anchors behind that direction cannot steer, attract, or
    /// catch the pointer during a short grace period.
    departure: Option<DepartureGuard>,
    /// Seconds left in the directional departure grace period.
    departure_for: f64,
    /// How hard a named or snapped target pulls, in radians a second.
    rate: f64,
    /// Seconds of uninterrupted directional thrust in the current gesture.
    /// Fresh input ramps from fine control to full acceleration.
    push_age: f64,
    /// The most recent continuous navigation mode. Free mode uses this after
    /// release to stop at the exact arbitrary coordinate instead of engaging
    /// magnetic capture.
    nav: NavMode,
    stepped: Instant,
}

impl Drift {
    fn toward(at: (f64, f64), to: (f64, f64), rate: f64) -> Self {
        Drift {
            at,
            vel: (0.0, 0.0),
            to: Some(to),
            magnet: None,
            departure: None,
            departure_for: 0.0,
            rate,
            push_age: 0.0,
            nav: NavMode::Normal,
            stepped: Instant::now(),
        }
    }

    fn free(at: (f64, f64)) -> Self {
        Drift {
            at,
            vel: (0.0, 0.0),
            to: None,
            magnet: None,
            departure: None,
            departure_for: 0.0,
            rate: config::get().pointer.spring(),
            push_age: 0.0,
            nav: NavMode::Normal,
            stepped: Instant::now(),
        }
    }

    fn speed(&self) -> f64 {
        self.vel.0.hypot(self.vel.1)
    }

    fn guard_departure(&mut self, anchor: (f64, f64), direction: (f64, f64)) {
        self.departure = Some(DepartureGuard::new(anchor, direction));
        self.departure_for = config::get().pointer.departure_ms as f64 / 1000.0;
    }

    fn release_target(&mut self, direction: (f64, f64)) {
        if let Some(departure) = self.departure.as_mut() {
            departure.aim(direction);
        }
        let released = self.to.take().or(self.magnet.take());
        self.magnet = None;
        if let Some(target) = released {
            self.guard_departure(target, direction);
            // A named or magnetic spring can be moving quickly in a
            // direction unrelated to the arrow the user just pressed. Free
            // navigation starts from rest; inertia belongs to arrow motion,
            // not to the target that was just cancelled.
            self.vel = (0.0, 0.0);
            self.push_age = 0.0;
        }
    }

    /// Carry the pointer forward by however long it has been since the last
    /// time, and say whether it has come to rest.
    fn step(
        &mut self,
        thrust: (f64, f64),
        mode: NavMode,
        anchors: &[(f64, f64)],
        wall: (f64, f64),
    ) -> bool {
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
            done = self.advance_as(dt, thrust, mode, anchors, wall);
            if done {
                break;
            }
        }
        done
    }

    /// The step itself, given how long it is. Apart from the clock this is
    /// all there is to the motion, so it is where the numbers can be checked.
    #[cfg(test)]
    fn advance(
        &mut self,
        dt: f64,
        thrust: (f64, f64),
        anchors: &[(f64, f64)],
        wall: (f64, f64),
    ) -> bool {
        self.advance_as(dt, thrust, NavMode::Normal, anchors, wall)
    }

    fn advance_as(
        &mut self,
        dt: f64,
        thrust: (f64, f64),
        mode: NavMode,
        anchors: &[(f64, f64)],
        wall: (f64, f64),
    ) -> bool {
        let cfg = &config::get().pointer;
        let pushing = thrust != (0.0, 0.0);
        if pushing {
            // Direct calls to the physics step get the same immediate arrow
            // takeover as the event loop. No magnetic velocity is allowed to
            // masquerade as a key that is still held.
            self.release_target(thrust);
            self.nav = mode;
        }
        if self.departure.is_some() {
            self.departure_for = (self.departure_for - dt).max(0.0);
        }
        if self.departure.is_some() && self.departure_for <= f64::EPSILON {
            // A short release must not immediately choose its source or an
            // anchor behind the requested direction. Once the grace period
            // ends, every anchor becomes eligible again.
            self.departure = None;
        }
        if !pushing && self.nav == NavMode::Free {
            self.to = None;
            self.magnet = None;
            self.vel = (0.0, 0.0);
            self.push_age = 0.0;
            return true;
        }
        let fresh_push = pushing && mode != NavMode::Free && self.push_age <= f64::EPSILON;
        let push_scale = if pushing && mode != NavMode::Free {
            self.push_age += dt;
            let span = cfg.ramp_ms as f64 / 1000.0;
            if span <= f64::EPSILON {
                1.0
            } else {
                let t = (self.push_age / span).clamp(0.0, 1.0);
                let eased = t * t * (3.0 - 2.0 * t);
                INITIAL_THRUST + (1.0 - INITIAL_THRUST) * eased
            }
        } else {
            self.push_age = 0.0;
            0.0
        };
        let reach = cfg.attract_px.max(cfg.snap_px).max(0.0);
        if self.to.is_none() && !pushing {
            if self.magnet.is_none() {
                // Use the drag-only stopping point so inertia decides between
                // overlapping anchors. Choosing only from the current point
                // makes a rightward release snap backward just before it
                // crosses the midpoint. There is deliberately no distance
                // cutoff: every release eventually lands on an anchor.
                let projected = if cfg.drag > f64::EPSILON {
                    (
                        self.at.0 + self.vel.0 / cfg.drag,
                        self.at.1 + self.vel.1 / cfg.drag,
                    )
                } else {
                    self.at
                };
                self.magnet =
                    nearest_point(projected, anchors, self.departure).map(|(anchor, _)| anchor);
            }
            if let Some(anchor) = self.magnet {
                let away = (anchor.0 - self.at.0).hypot(anchor.1 - self.at.1);
                let catch = cfg.snap_px.max(1.5);
                if away <= catch {
                    self.to = Some(anchor);
                    self.magnet = None;
                    self.rate = cfg.spring();
                }
            }
        }

        match self.to {
            // A spring damped a little under the point where it would stop
            // dead. The small overshoot is the tension in the arrival.
            Some((tx, ty)) => {
                let w = self.rate;
                let (dx, dy) = (tx - self.at.0, ty - self.at.1);
                self.vel.0 += (dx * w * w - self.vel.0 * 1.35 * w) * dt;
                self.vel.1 += (dy * w * w - self.vel.1 * 1.35 * w) * dt;
            }
            // Free motion has direct acceleration and a speed ceiling. When
            // the keys are released, drag spends the velocity gradually.
            None => {
                if pushing && mode == NavMode::Free {
                    let speed = cfg.direct_speed_px.max(1.0);
                    self.vel = (thrust.0 * speed, thrust.1 * speed);
                } else {
                    if fresh_push {
                        let along = self.vel.0 * thrust.0 + self.vel.1 * thrust.1;
                        let launch = cfg.launch_speed_px.max(0.0).min(cfg.speed_px.max(1.0));
                        if along < launch {
                            self.vel.0 += thrust.0 * (launch - along);
                            self.vel.1 += thrust.1 * (launch - along);
                        }
                    }
                    self.vel.0 += thrust.0 * cfg.accel_px.max(0.0) * push_scale * dt;
                    self.vel.1 += thrust.1 * cfg.accel_px.max(0.0) * push_scale * dt;
                }

                if pushing && mode == NavMode::Normal {
                    // A held arrow remains in control. The nearest anchor can
                    // bend its path, but cannot catch it or overpower it.
                    if let Some(((tx, ty), away)) = nearest_point(self.at, anchors, self.departure)
                    {
                        let speed = self.speed();
                        if away > f64::EPSILON && away < reach && speed > 5.0 {
                            let influence = (1.0 - away / reach).sqrt();
                            let moving = (speed / cfg.speed_px.max(1.0)).clamp(0.0, 1.0);
                            let pull = cfg.accel_px.max(0.0) * 0.12 * influence * moving;
                            self.vel.0 += (tx - self.at.0) / away * pull * dt;
                            self.vel.1 += (ty - self.at.1) / away * pull * dt;
                        }
                    }
                } else if let Some((tx, ty)) = self.magnet {
                    // Released motion behaves like a stretched spring. Pull
                    // grows with distance well beyond the softness radius,
                    // so empty-space travel starts decisively while the
                    // close approach remains controllable.
                    let away = (tx - self.at.0).hypot(ty - self.at.1);
                    if away > f64::EPSILON {
                        let stretch = (away / reach.max(1.0)).min(4.0);
                        let magnetic_accel = cfg.accel_px.max(cfg.speed_px.max(1.0) * 2.0);
                        let pull = magnetic_accel * 0.70 * stretch;
                        self.vel.0 += (tx - self.at.0) / away * pull * dt;
                        self.vel.1 += (ty - self.at.1) / away * pull * dt;
                    }
                }

                if !pushing {
                    // Magnetic travel keeps more momentum than a free coast,
                    // so a distant anchor remains reachable without turning
                    // the release into a long crawl.
                    let drag = if self.magnet.is_some() {
                        cfg.drag.max(0.0) * 0.52
                    } else {
                        cfg.drag.max(0.0)
                    };
                    let keep = (-drag * dt).exp();
                    self.vel.0 *= keep;
                    self.vel.1 *= keep;
                }
                let top = match mode {
                    NavMode::Free => cfg.direct_speed_px.max(1.0),
                    _ => cfg.speed_px.max(1.0),
                };
                let speed = self.speed();
                if speed > top {
                    self.vel.0 *= top / speed;
                    self.vel.1 *= top / speed;
                }
            }
        }

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
        match self.to {
            Some(to)
                if (to.0 - self.at.0).hypot(to.1 - self.at.1) <= cfg.snap_px.max(1.5)
                    && self.speed() <= (cfg.speed_px.max(1.0) * 0.18).max(25.0) =>
            {
                self.at = to;
                self.vel = (0.0, 0.0);
                true
            }
            Some(_) => false,
            None => {
                !pushing && self.magnet.is_none() && self.departure.is_none() && self.speed() < 5.0
            }
        }
    }
}

/// The closest anchor to a point, and the distance to it.
fn nearest_point(
    at: (f64, f64),
    anchors: &[(f64, f64)],
    departure: Option<DepartureGuard>,
) -> Option<((f64, f64), f64)> {
    anchors
        .iter()
        .copied()
        .filter(|point| departure.is_none_or(|guard| !guard.excludes(*point)))
        .map(|p| (p, (p.0 - at.0).hypot(p.1 - at.1)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
}

/// Choose the next visual box in one cardinal direction. Distance along the
/// requested axis wins, with sideways drift penalized enough to keep a row
/// or column straight. Vertical movement may continue in reading order when
/// no box is directly in line.
fn pull(
    from: Rect,
    (dx, dy): (f64, f64),
    boxes: &[Rect],
    reach: f64,
    wander: bool,
) -> Option<usize> {
    const ACROSS: f64 = 2.0;
    const SPREAD: (f64, f64) = (40.0, 0.3);

    let gap = |(a0, a1): (f64, f64), (b0, b1): (f64, f64)| (b0 - a1).max(a0 - b1).max(0.0);
    let span = |r: &Rect, across: bool| match across {
        true if dx != 0.0 => (r.y as f64, (r.y + r.h) as f64),
        true => (r.x as f64, (r.x + r.w) as f64),
        false if dx != 0.0 => (r.x as f64, (r.x + r.w) as f64),
        false => (r.y as f64, (r.y + r.h) as f64),
    };
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
                false => (-(r.y + r.h) as f64, -(r.x + r.w) as f64),
            };
            let (a, b) = (key(a), key(b));
            a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1))
        })
        .map(|(i, _)| i)
}

/// Keep a keyboard-driven pointer inside its active window and report each
/// outward direction that hit an edge. A diagonal can therefore request one
/// horizontal and one vertical scroll event without increasing pointer speed.
fn constrain_to_window(
    at: &mut (f64, f64),
    vel: &mut (f64, f64),
    thrust: (f64, f64),
    (left, top, right, bottom): (f64, f64, f64, f64),
) -> Vec<Arrow> {
    let mut hit = Vec::with_capacity(2);
    let (min_x, max_x) = (
        left + EDGE_INSET,
        (right - EDGE_INSET).max(left + EDGE_INSET),
    );
    let (min_y, max_y) = (
        top + EDGE_INSET,
        (bottom - EDGE_INSET).max(top + EDGE_INSET),
    );
    if thrust.0 < 0.0 && at.0 <= min_x {
        at.0 = min_x;
        vel.0 = 0.0;
        hit.push(Arrow::Left);
    } else if thrust.0 > 0.0 && at.0 >= max_x {
        at.0 = max_x;
        vel.0 = 0.0;
        hit.push(Arrow::Right);
    }
    if thrust.1 < 0.0 && at.1 <= min_y {
        at.1 = min_y;
        vel.1 = 0.0;
        hit.push(Arrow::Up);
    } else if thrust.1 > 0.0 && at.1 >= max_y {
        at.1 = max_y;
        vel.1 = 0.0;
        hit.push(Arrow::Down);
    }
    hit
}

/// Left, right, up, and down as one direction. Opposites cancel and a
/// diagonal is normalized, so adding a second key changes direction without
/// adding speed.
fn arrow_direction(held: [bool; 4]) -> (f64, f64) {
    let axis = |less: usize, more: usize| f64::from(held[more]) - f64::from(held[less]);
    let (x, y) = (axis(0, 1), axis(2, 3));
    let len = x.hypot(y);
    match len > 0.0 {
        true => (x / len, y / len),
        false => (0.0, 0.0),
    }
}

/// The first repeat from Hyprland arrives after the keyboard repeat delay,
/// which is 600ms by default. Once repeats start, they arrive every 40ms at
/// the default rate. These leases let a real hold bridge that first delay,
/// then stop shortly after repeat pulses stop even if every release is lost.
const INITIAL_ARROW_LEASE: Duration = Duration::from_millis(750);
const REPEATED_ARROW_LEASE: Duration = Duration::from_millis(120);

/// The four arrow keys as one direction. Each release removes only its own
/// axis, which lets a diagonal turn into a cardinal hold without a stop.
/// Leases remain as a last resort if a compositor drops every release edge.
#[derive(Default)]
struct ArrowKeys {
    held: [Option<NavMode>; 4],
    mode: NavMode,
    expires: Option<Instant>,
}

impl ArrowKeys {
    /// Record one edge or repeat pulse and report whether the set changed.
    fn set(&mut self, which: usize, mode: NavMode, down: bool, now: Instant) -> bool {
        if down {
            self.mode = mode;
            if self.held[which] == Some(mode) {
                // Repeating arrow binds refresh a short lease. Hyprland stops
                // their timer on any physical key edge, independently of
                // whether its global-shortcut release reaches this client.
                self.expires = Some(now + REPEATED_ARROW_LEASE);
                return false;
            }
            self.held[which] = Some(mode);
            self.expires = Some(now + INITIAL_ARROW_LEASE);
        } else {
            // A modifier may change while the physical arrow remains down.
            // A late release for the old binding must not cancel the newer
            // mode that replaced it.
            if self.held[which] != Some(mode) {
                return false;
            }
            self.held[which] = None;
            if self.any() {
                // The compositor may take a moment to resume repeats for a
                // key that was held before the one just released.
                self.expires = Some(now + INITIAL_ARROW_LEASE);
            } else {
                self.expires = None;
            }
        }
        true
    }

    fn motion(&mut self, now: Instant) -> ((f64, f64), NavMode) {
        if self.expires.is_some_and(|expires| now >= expires) {
            self.clear();
        }
        if !self.any() {
            return ((0.0, 0.0), NavMode::Normal);
        }
        (
            arrow_direction(self.held.map(|held| held.is_some())),
            self.mode,
        )
    }

    fn any(&self) -> bool {
        self.held.iter().any(Option::is_some)
    }

    /// The fallback keyboard path does not retain the modifier state from a
    /// key's press. Its physical release still ends that direction safely.
    fn release_any(&mut self, which: usize, now: Instant) -> bool {
        let Some(mode) = self.held[which] else {
            return false;
        };
        self.set(which, mode, false, now)
    }

    fn clear(&mut self) {
        self.held.fill(None);
        self.mode = NavMode::Normal;
        self.expires = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ButtonEdge {
    button: u32,
    down: bool,
}

/// Mouse buttons driven by click-key edges. State changes are queued because
/// key callbacks cannot borrow the virtual pointer that lives in the run loop.
#[derive(Default)]
struct ButtonInput {
    held: Vec<u32>,
    pending: VecDeque<ButtonEdge>,
    finish: bool,
}

impl ButtonInput {
    fn press(&mut self, button: u32) -> bool {
        if self.held.contains(&button) {
            return false;
        }
        self.held.push(button);
        self.pending.push_back(ButtonEdge { button, down: true });
        true
    }

    fn release(&mut self, button: u32) -> bool {
        let Some(i) = self.held.iter().position(|held| *held == button) else {
            return false;
        };
        self.held.swap_remove(i);
        self.pending.push_back(ButtonEdge {
            button,
            down: false,
        });
        self.finish = true;
        true
    }

    fn release_all(&mut self) {
        for button in self.held.drain(..) {
            self.pending.push_back(ButtonEdge {
                button,
                down: false,
            });
        }
    }

    fn next(&mut self) -> Option<ButtonEdge> {
        self.pending.pop_front()
    }

    fn any(&self) -> bool {
        !self.held.is_empty()
    }

    fn take_finish(&mut self) -> bool {
        std::mem::take(&mut self.finish)
    }
}

/// What a complete hint or tile picked out.
#[derive(Clone, Copy, PartialEq, Debug)]
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
    subcompositor_state: SubcompositorState,
    layer: Option<LayerSurface>,
    /// A tiny subsurface that carries the moving focus ring. Repositioning it
    /// does not repaint the full-output overlay.
    pointer_overlay: Option<IndicatorOverlay>,
    /// A second tiny subsurface that covers the nearest red anchor dot with a
    /// blue one. It moves independently of the cached full-output frame.
    anchor_overlay: Option<IndicatorOverlay>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    keyboard_modifiers: Modifiers,
    snap: Snapshot,
    font: Font,
    buffer_scale: i32,
    width: u32,
    height: u32,
    configured: bool,
    dirty: bool,
    stage: Stage,
    exit: bool,
    /// Where an instant hint selection clicks on the way out.
    target: Option<(f64, f64)>,
    /// Where to send the pointer once the loop can get to it.
    aim: Option<(f64, f64)>,
    /// An anchor jump waiting to be sent without spring travel.
    teleport: Option<(Target, (f64, f64))>,
    /// Cancel motion at the next safe point outside an event callback.
    stop_motion: bool,
    hints: Vec<Hint>,
    /// Keys already confirmed, narrowing the hints.
    typed: String,
    /// Live mouse-button state and edges waiting for the virtual pointer.
    buttons: ButtonInput,
    /// Drills exercise navigation without sending button events to a window.
    buttons_enabled: bool,
    /// A scroll waiting for the loop to send it, and when to look at the
    /// window again once the scrolling stops.
    scroll: VecDeque<Wheel>,
    /// A Shift-arrow press is live, so repeats do not release Shift in the
    /// client again until the physical arrow is released.
    shift_scroll_held: bool,
    /// Release the client's Shift state before sending the next scroll frame.
    scroll_needs_unshift: bool,
    settle: Option<Instant>,
    /// Scrolling pins the compact anchor view until an alphabetic key asks
    /// for labels again.
    hint_display: HintDisplay,
    /// Alphabetic input received before the refreshed hints are ready. Replay
    /// it after settling so the key that wakes labels is not discarded.
    deferred_letters: VecDeque<char>,
    /// One cooldown per edge, in left, right, up, down order.
    edge_scrolled: [Option<Instant>; 4],
    /// Says how far the window moved under the overlay, wheel or otherwise.
    watch: Option<std::sync::mpsc::Receiver<(f64, f64)>>,
    /// With `keys.confirm` on, a key pressed once and not taken yet: it shows
    /// what it would select, and pressing it again takes it.
    armed: Option<char>,
    /// What is highlighted and its selected coordinate. During free movement
    /// the target is the nearest anchor but the point is the pointer itself.
    picked: Option<(Target, (f64, f64))>,
    /// What the pointer is nearest at this moment, which is what is drawn
    /// lit. The same thing as `picked` once the pointer has come to rest.
    lit: Option<Target>,
    /// Which arrow keys are live: left, right, up, down. They are combined
    /// into one direction so a chord is one movement, not two independent
    /// repeat queues.
    arrows: ArrowKeys,
    /// Whether arrow navigation has released the pointer from an anchor.
    /// This remains true while it coasts and settles into its magnetic target.
    free: bool,
    /// Where the pointer is right now, which while it is travelling is
    /// somewhere between two targets and worth drawing.
    pointer: Option<(f64, f64)>,
    /// A pre-rendered compact frame. Arrow input swaps this onto the main
    /// surface without painting the output in the middle of motion.
    navigation_buffer: Option<Buffer>,
    /// Whether the last full frame used the compact anchor-dot fallback.
    showed_dots: bool,
    /// When keyboard motion was last active. The full overlay remains stable
    /// during motion; this drives the compact fallback when needed.
    navigating: Option<Instant>,
    elements_cache: HashMap<usize, Vec<Element>>,
    pending_pick: Option<usize>,
    /// Whether nothing has happened since the overlay opened or was last
    /// reset, which is what lets the reset key close it.
    fresh: bool,
    /// One clock shared by key-edge and motion traces, so an end-to-end run
    /// can line compositor delivery up with the pointer response.
    trace_started: Instant,
    /// An element query already in flight, so the wait for it overlaps the
    /// rest of the startup instead of following it.
    query: Option<Query>,
}

struct IndicatorOverlay {
    surface: wl_surface::WlSurface,
    subsurface: wl_subsurface::WlSubsurface,
    visible: bool,
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
        let (pid, title, size) = (w.pid, w.title.clone(), (w.w, w.h));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ =
                tx.send(atspi::clickable_elements(pid, &title, size).map_err(|e| e.to_string()));
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

/// Show the overlay and return the final global click or drag position, or
/// None if the user cancelled.
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
    let subcompositor_state =
        SubcompositorState::bind(compositor.wl_compositor().clone(), &globals, &qh)?;
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
        subcompositor_state,
        layer: None,
        pointer_overlay: None,
        anchor_overlay: None,
        keyboard: None,
        keyboard_modifiers: Modifiers::default(),
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
        aim: None,
        teleport: None,
        stop_motion: false,
        hints: Vec::new(),
        typed: String::new(),
        buttons: ButtonInput::default(),
        buttons_enabled: drill.is_none(),
        scroll: VecDeque::new(),
        shift_scroll_held: false,
        scroll_needs_unshift: false,
        settle: None,
        hint_display: HintDisplay::default(),
        deferred_letters: VecDeque::new(),
        edge_scrolled: [None; 4],
        watch: None,
        armed: None,
        picked: None,
        lit: None,
        arrows: ArrowKeys::default(),
        free: false,
        pointer: None,
        navigation_buffer: None,
        showed_dots: false,
        navigating: None,
        elements_cache: HashMap::new(),
        pending_pick,
        fresh: true,
        trace_started: Instant::now(),
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
    let pointer_parent = surface.clone();
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

    // The nearest-anchor dot and focus ring move on small surfaces. Moving
    // either child only commits surface state; it does not clear and repaint
    // the output-sized parent buffer. Create the focus ring last so it stays
    // above the blue dot when both occupy the same point.
    let (anchor_subsurface, anchor_surface) = app
        .subcompositor_state
        .create_subsurface(pointer_parent.clone(), &qh);
    anchor_subsurface.set_desync();
    let empty = compositor.wl_compositor().create_region(&qh, ());
    anchor_surface.set_input_region(Some(&empty));
    empty.destroy();
    app.anchor_overlay = Some(IndicatorOverlay {
        surface: anchor_surface,
        subsurface: anchor_subsurface,
        visible: false,
    });

    let (pointer_subsurface, pointer_surface) = app
        .subcompositor_state
        .create_subsurface(pointer_parent, &qh);
    pointer_subsurface.set_desync();
    let empty = compositor.wl_compositor().create_region(&qh, ());
    pointer_surface.set_input_region(Some(&empty));
    empty.destroy();
    app.pointer_overlay = Some(IndicatorOverlay {
        surface: pointer_surface,
        subsurface: pointer_subsurface,
        visible: false,
    });

    // One virtual pointer for the whole run: it sends live motion and button
    // edges while the overlay is up, and handles an instant click after it
    // comes down.
    let pointer = vp_manager.as_ref().map(|mgr| {
        let seat = app.seat_state.seats().next();
        mgr.create_virtual_pointer(seat.as_ref(), &qh, ())
    });

    // Where the pointer is, so a target can be travelled to rather than
    // jumped to, and so a hand on the mouse can be told from the overlay's
    // own doing.
    let mut pointer_at = hypr::cursor_pos();
    let mut live_target = None;
    let mut drift: Option<Drift> = None;
    // Where the pointer was last seen to be, once it had stopped being moved
    // from here. Compared against itself rather than against where it was
    // last sent, so that the compositor lagging a step behind cannot read as
    // a hand on the mouse.
    let mut resting: Option<(f64, f64)> = None;
    let mut looked = Instant::now();
    let mut painted_once = false;

    // A drill presses its own keys on a schedule and says what happened.
    let drilling = drill.is_some();
    let mut script = drill.map(|d| d.steps.into_iter()).into_iter().flatten();
    // Keys, release time, and the next synthetic arrow repeat. The drill
    // mirrors Hyprland's default 600ms delay and 25Hz repeat rate so long
    // scripted holds exercise the same leases as physical holds.
    let mut pending: Option<(Vec<Key>, Instant, Instant)> = None;
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
                let wait = if let Some(drift) = drift.as_ref() {
                    MOTION_FRAME.saturating_sub(drift.stepped.elapsed())
                } else if app.settle.is_some() {
                    MOTION_FRAME
                } else if app.watch.is_some() {
                    // Often enough to catch a scroll that went past the
                    // overlay; the check itself is one message.
                    Duration::from_millis(40)
                } else if drilling && pending.is_some() {
                    Duration::from_millis(20)
                } else if app.navigating.is_some() {
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
                if pending.as_ref().is_some_and(|(_, until, _)| now >= *until) {
                    if let Some((keys, _, _)) = pending.take() {
                        for key in keys {
                            app.release(key);
                        }
                    }
                    next_step = now + Duration::from_millis(600);
                }
                if let Some((keys, until, repeat_at)) = pending.as_mut() {
                    if now < *until && now >= *repeat_at {
                        if let Some(&key) = keys.iter().rev().find(|key| key.repeats()) {
                            app.press(key);
                        }
                        *repeat_at = now + Duration::from_millis(40);
                    }
                }
                // And take the next step when the last one has settled.
                if pending.is_none() && now >= next_step {
                    match script.next() {
                        Some((keys, how_long)) if keys.is_empty() => {
                            next_step = now + how_long;
                        }
                        Some((keys, how_long)) => {
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
                            eprintln!("step {step_no}: {keys:?} held {}ms", how_long.as_millis());
                            was = Some((at, now_at));
                            for &key in &keys {
                                app.press(key);
                            }
                            pending =
                                Some((keys, now + how_long, now + Duration::from_millis(600)));
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
            let mut sent_button = false;
            while let Some(edge) = app.buttons.next() {
                let vp = pointer
                    .as_ref()
                    .ok_or("compositor does not expose zwlr_virtual_pointer_manager_v1")?;
                send_button(vp, edge);
                sent_button = true;
            }
            if sent_button {
                queue.flush()?;
            }
            if app.buttons.take_finish() {
                // The release is already on the wire. Stop before another
                // motion frame can move the pointer after the drag ended.
                live_target = pointer_at;
                app.exit = true;
                continue;
            }
            if let Some(vp) = pointer.as_ref() {
                if app.scroll_needs_unshift && !app.scroll.is_empty() {
                    app.scroll_needs_unshift = false;
                    if let Err(e) = hypr::release_shift_for_scroll() {
                        eprintln!("scroll modifier: {e}");
                        // Let the next repeat retry instead of turning one
                        // transient IPC failure into a dead held gesture.
                        app.shift_scroll_held = false;
                    }
                }
                while let Some(wheel) = app.scroll.pop_front() {
                    send_scroll(vp, wheel);
                }
                queue.flush()?;
            }
            // Someone scrolled the window without going through the overlay,
            // which the hints have no way of knowing on their own.
            while let Some(moved) = app.watch.as_ref().and_then(|rx| rx.try_recv().ok()) {
                app.shifted(moved);
            }
            // Everything has moved, so the hints have to be read again. Once
            // the scrolling stops, not on every press of a key held down.
            if app.settle.is_some_and(|at| Instant::now() >= at)
                && !app.any_arrow()
                && drift.is_none()
            {
                app.resettle();
            }
            // A named target wants the pointer on it. Doing that here rather
            // than where it was picked keeps the requests and their roundtrip
            // out of the middle of an event callback.
            if app.stop_motion {
                drift = None;
                app.stop_motion = false;
            }
            if let Some((what, to)) = app.teleport.take() {
                drift = None;
                pointer_at = Some(to);
                if let Some(vp) = pointer.as_ref() {
                    send_pointer_motion(vp, to, app.snap.layout_extent);
                    queue.flush()?;
                }
                app.land_target(what, to);
                resting = None;
            }
            if let Some(to) = app.aim.take() {
                match (drift.as_mut(), pointer_at) {
                    // Already moving: let the spring catch it without taking
                    // away the speed it has.
                    (Some(d), _) => {
                        d.magnet = None;
                        d.to = Some(to);
                        d.rate = config::get().pointer.spring();
                    }
                    (None, Some(from)) => {
                        drift = Some(Drift::toward(from, to, config::get().pointer.spring()))
                    }
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

            // Every arrow currently down becomes one normalized direction.
            // Starting free motion once per chord leaves no queued horizontal
            // or vertical repeats to carry on after the chord is released.
            let (thrust, nav_mode) = app.motion();
            if app.any_arrow() {
                match drift.as_mut() {
                    Some(d) => d.release_target(thrust),
                    None => {
                        let from = pointer_at.unwrap_or_default();
                        let mut next = Drift::free(from);
                        if let Some((_, anchor, _)) =
                            app.nearest(from).filter(|(_, _, away)| *away <= CAUGHT)
                        {
                            next.guard_departure(anchor, thrust);
                        }
                        drift = Some(next);
                    }
                }
            }
            let motion_due = drift
                .as_ref()
                .is_some_and(|drift| drift.stepped.elapsed() >= MOTION_FRAME);
            if let (true, Some(vp), Some(d)) = (motion_due, pointer.as_ref(), drift.as_mut()) {
                // The pointer is being moved from here, so where it is says
                // nothing about hands.
                looked = Instant::now();
                resting = None;
                let extent = app.snap.layout_extent;
                let wall = ((extent.0 - 1) as f64, (extent.1 - 1) as f64);
                let anchors: Vec<(f64, f64)> =
                    app.targets().into_iter().map(|(_, at)| at).collect();
                let frame_gap = d.stepped.elapsed().as_secs_f64() * 1000.0;
                let done = d.step(thrust, nav_mode, &anchors, wall);
                let edges = app
                    .navigation_bounds()
                    .map(|bounds| constrain_to_window(&mut d.at, &mut d.vel, thrust, bounds))
                    .unwrap_or_default();
                for way in edges {
                    app.queue_edge_scroll(way);
                }
                if std::env::var_os("WL_TRACE").is_some() {
                    let state = if d.to.is_some() {
                        "spring"
                    } else if d.magnet.is_some() {
                        "attract"
                    } else if thrust != (0.0, 0.0) && nav_mode == NavMode::Free {
                        "direct"
                    } else {
                        "free"
                    };
                    let active = d.to.or(d.magnet).map(|at| {
                        let away = (at.0 - d.at.0).hypot(at.1 - d.at.1);
                        (at, away)
                    });
                    let (anchor, distance) = active
                        .or_else(|| app.nearest(d.at).map(|(_, at, away)| (at, away)))
                        .map(|(at, away)| (format!("({:.2},{:.2})", at.0, at.1), away))
                        .unwrap_or_else(|| ("none".into(), f64::NAN));
                    eprintln!(
                        "NAV frame t={} gap={frame_gap:.2} input=({:.3},{:.3}) \
                         at=({:.2},{:.2}) velocity=({:.2},{:.2}) speed={:.2} \
                         state={state} anchor={anchor} distance={distance:.2} done={done}",
                        app.trace_started.elapsed().as_millis(),
                        thrust.0,
                        thrust.1,
                        d.at.0,
                        d.at.1,
                        d.vel.0,
                        d.vel.1,
                        d.speed(),
                    );
                }
                send_pointer_motion(vp, d.at, extent);
                queue.flush()?;
                pointer_at = Some(d.at);
                app.pointer = Some(d.at);
                if app.free {
                    app.show_pointer_indicator(true);
                    app.navigating = Some(Instant::now());
                }
                // Track the nearest anchor in state, but do not repaint the
                // full output for it. The pointer subsurface above is the
                // live frame; the labels catch up once motion stops.
                app.follow_pointer(d.at);
                if app.free {
                    app.update_anchor_indicator(d.at);
                    // This parent commit applies both small subsurface
                    // positions in one frame.
                    app.move_pointer_indicator(d.at);
                }
                if done {
                    if app.free {
                        if let Some(to) = d.to {
                            app.land(to);
                        }
                    }
                    drift = None;
                }
            }
            // An expired navigation timer must not keep the idle loop polling
            // when scrolling has independently pinned the anchor view.
            if app
                .navigating
                .is_some_and(|t| t.elapsed() >= config::get().label.wake())
            {
                app.navigating = None;
            }
            let dots = app.dots();
            if app.showed_dots != dots {
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
                // A full-output redraw stalls pointer updates for tens of
                // milliseconds. Paint the first frame, then leave it stable
                // throughout a flight. The small pointer subsurface is the
                // only surface that needs to move live.
                if drift.is_none() || !painted_once || app.showed_dots != app.dots() {
                    app.draw();
                    painted_once = true;
                }
            }
        }
    }

    // A cancelled run may leave a physical click key down. Release every
    // virtual button before the pointer disappears so the window below never
    // inherits a stuck drag.
    app.buttons.release_all();
    let mut released_button = false;
    while let Some(edge) = app.buttons.next() {
        if let Some(vp) = pointer.as_ref() {
            send_button(vp, edge);
            released_button = true;
        }
    }
    if released_button {
        queue.roundtrip(&mut app)?;
    }

    // Unmap the overlay before an instant hint selection clicks. Dropping the
    // shortcuts first puts the keyboard back the way it was.
    drop(shortcuts);
    if let Some(pointer) = app.pointer_overlay.take() {
        pointer.subsurface.destroy();
        pointer.surface.destroy();
    }
    if let Some(anchor) = app.anchor_overlay.take() {
        anchor.subsurface.destroy();
        anchor.surface.destroy();
    }
    app.layer.take();
    queue.roundtrip(&mut app)?;

    if let Some(target) = app.target {
        let vp = pointer
            .as_ref()
            .ok_or("compositor does not expose zwlr_virtual_pointer_manager_v1")?;
        let extent = app.snap.layout_extent;
        move_and_click(vp, target, extent, Some(BTN_LEFT), &mut queue, &mut app)?;
        vp.destroy();
        queue.roundtrip(&mut app)?;
    }
    Ok(app.target.or(live_target))
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

/// Scroll whatever the pointer is on. The overlay takes no pointer input, so
/// this reaches the window underneath.
fn send_scroll(vp: &ZwlrVirtualPointerV1, wheel: Wheel) {
    let axis = match wheel.across {
        true => wl_pointer::Axis::HorizontalScroll,
        false => wl_pointer::Axis::VerticalScroll,
    };
    let way = if wheel.back { -1.0 } else { 1.0 };
    vp.axis_source(wl_pointer::AxisSource::Wheel);
    // A notch is fifteen units by convention, and the discrete count is what
    // an application that counts wheel clicks reads.
    let mut left = config::get().scroll.step.max(1) as i32;
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

fn send_pointer_motion(
    vp: &ZwlrVirtualPointerV1,
    (gx, gy): (f64, f64),
    (ext_w, ext_h): (i32, i32),
) {
    let x = gx.round().clamp(0.0, (ext_w - 1) as f64) as u32;
    let y = gy.round().clamp(0.0, (ext_h - 1) as f64) as u32;
    vp.motion_absolute(0, x, y, ext_w as u32, ext_h as u32);
    vp.frame();
}

fn send_button(vp: &ZwlrVirtualPointerV1, edge: ButtonEdge) {
    let state = match edge.down {
        true => wl_pointer::ButtonState::Pressed,
        false => wl_pointer::ButtonState::Released,
    };
    vp.button(0, edge.button, state);
    vp.frame();
}

fn move_and_click<S>(
    vp: &ZwlrVirtualPointerV1,
    at: (f64, f64),
    extent: (i32, i32),
    click: Option<u32>,
    queue: &mut wayland_client::EventQueue<S>,
    state: &mut S,
) -> Result<(), Box<dyn Error>> {
    send_pointer_motion(vp, at, extent);
    queue.roundtrip(state)?;
    if let Some(button) = click {
        send_button(vp, ButtonEdge { button, down: true });
        send_button(
            vp,
            ButtonEdge {
                button,
                down: false,
            },
        );
        queue.roundtrip(state)?;
    }
    Ok(())
}

impl App {
    fn trace_key(&self, edge: &str, key: Key) {
        if std::env::var_os("WL_KEYS").is_some() {
            eprintln!(
                "NAV key t={} edge={edge} key={}",
                self.trace_started.elapsed().as_millis(),
                key.name()
            );
        }
    }

    /// Map or clear the small focus-ring surface. This allocates only a
    /// 48-pixel logical square, unlike a normal overlay redraw.
    fn show_pointer_indicator(&mut self, show: bool) {
        let Some(pointer) = self.pointer_overlay.as_ref() else {
            return;
        };
        if pointer.visible == show {
            return;
        }
        let surface = pointer.surface.clone();
        let scale = self.buffer_scale;
        let side = POINTER_SURFACE_SIZE * scale;
        let (buffer, buf) =
            match self
                .pool
                .create_buffer(side, side, side * 4, wl_shm::Format::Argb8888)
            {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("pointer buffer allocation failed: {e}");
                    return;
                }
            };
        let mut canvas = Canvas {
            buf,
            w: side,
            h: side,
        };
        canvas.clear(Color::new(0.0, 0.0, 0.0, 0.0));
        if show {
            let center = side as f32 / 2.0;
            draw_pointer_mark(&mut canvas, center, center, scale);
        }
        surface.set_buffer_scale(scale);
        if buffer.attach_to(&surface).is_err() {
            eprintln!("failed to attach pointer buffer");
            return;
        }
        surface.damage_buffer(0, 0, side, side);
        surface.commit();
        if let Some(pointer) = self.pointer_overlay.as_mut() {
            pointer.visible = show;
        }
    }

    /// Map or clear the blue nearest-anchor dot. Like the focus ring, this is
    /// a tiny buffer and never repaints the full navigation frame.
    fn show_anchor_indicator(&mut self, show: bool) {
        let Some(anchor) = self.anchor_overlay.as_ref() else {
            return;
        };
        if anchor.visible == show {
            return;
        }
        let surface = anchor.surface.clone();
        let scale = self.buffer_scale;
        let side = ANCHOR_SURFACE_SIZE * scale;
        let (buffer, buf) =
            match self
                .pool
                .create_buffer(side, side, side * 4, wl_shm::Format::Argb8888)
            {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("anchor buffer allocation failed: {e}");
                    return;
                }
            };
        let mut canvas = Canvas {
            buf,
            w: side,
            h: side,
        };
        canvas.clear(Color::new(0.0, 0.0, 0.0, 0.0));
        if show {
            let center = side as f32 / 2.0;
            draw_nearest_anchor_mark(&mut canvas, center, center, scale);
        }
        surface.set_buffer_scale(scale);
        if buffer.attach_to(&surface).is_err() {
            eprintln!("failed to attach anchor buffer");
            return;
        }
        surface.damage_buffer(0, 0, side, side);
        surface.commit();
        if let Some(anchor) = self.anchor_overlay.as_mut() {
            anchor.visible = show;
        }
    }

    /// Put the blue dot on the anchor currently nearest the free pointer.
    /// The following focus-ring move commits both synchronized positions.
    fn update_anchor_indicator(&mut self, pointer_at: (f64, f64)) {
        if !self.free {
            self.show_anchor_indicator(false);
            return;
        }
        let Some((_, at, _)) = self.nearest(pointer_at) else {
            self.show_anchor_indicator(false);
            return;
        };
        self.show_anchor_indicator(true);
        let Some(anchor) = self.anchor_overlay.as_ref() else {
            return;
        };
        let mon = &self.snap.monitor;
        let half = ANCHOR_SURFACE_SIZE as f64 / 2.0;
        let x = (at.0 - mon.x as f64 - half).round() as i32;
        let y = (at.1 - mon.y as f64 - half).round() as i32;
        anchor.subsurface.set_position(x, y);
    }

    /// Reposition the focus ring by committing only subsurface state. The
    /// parent label or anchor-dot frame stays untouched.
    fn move_pointer_indicator(&self, (px, py): (f64, f64)) {
        let (Some(pointer), Some(layer)) = (&self.pointer_overlay, &self.layer) else {
            return;
        };
        if !pointer.visible {
            return;
        }
        let mon = &self.snap.monitor;
        let half = POINTER_SURFACE_SIZE as f64 / 2.0;
        let x = (px - mon.x as f64 - half).round() as i32;
        let y = (py - mon.y as f64 - half).round() as i32;
        pointer.subsurface.set_position(x, y);
        // Subsurface positions are synchronized with the parent even when
        // the child buffer itself is desynchronized.
        layer.wl_surface().commit();
    }

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

    /// The mouse button a click key drives.
    fn click_button(key: Key) -> Option<u32> {
        match key {
            Key::LeftClick => Some(BTN_LEFT),
            Key::RightClick => Some(BTN_RIGHT),
            _ => None,
        }
    }

    /// Scroll the window under the pointer and pin the compact anchor view.
    /// The hints are refreshed once scrolling settles, but labels wait for
    /// alphabetic input before they replace the anchors.
    fn roll(&mut self, wheel: Wheel) {
        if matches!(self.stage, Stage::PickWindow) {
            return;
        }
        self.scroll.push_back(wheel);
        self.shifted((0.0, 0.0));
        if !self.show_navigation_frame() {
            self.dirty = true;
        }
    }

    fn navigation_bounds(&self) -> Option<(f64, f64, f64, f64)> {
        let win = match self.stage {
            Stage::PickHint { win } | Stage::PickTile { win } => win,
            Stage::PickWindow => return None,
        };
        self.snap.windows.get(win).map(|w| {
            (
                w.x as f64,
                w.y as f64,
                (w.x + w.w - 1) as f64,
                (w.y + w.h - 1) as f64,
            )
        })
    }

    fn queue_edge_scroll(&mut self, way: Arrow) {
        let now = Instant::now();
        let last = &mut self.edge_scrolled[way.index()];
        if last.is_some_and(|then| now.duration_since(then) < EDGE_SCROLL_INTERVAL) {
            return;
        }
        *last = Some(now);
        self.roll(way.wheel());
    }

    /// The window moved under the overlay: show anchors while their positions
    /// follow the content, then read them again once it stops moving.
    fn shifted(&mut self, (dx, dy): (f64, f64)) {
        if matches!(self.stage, Stage::PickWindow) {
            return;
        }
        self.settle = Some(Instant::now() + config::get().scroll.settle());
        self.hint_display.scrolled();
        self.typed.clear();
        self.armed = None;
        self.dirty = true;
        // Carry the anchors along rather than leave them behind. Whatever the
        // document moved by, everything in it moved by, so they can follow
        // within a frame instead of waiting out the exact refresh.
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        for h in self.hints.iter_mut().filter(|h| h.scrolls) {
            h.rx += dx;
            h.ry += dy;
            h.cx += dx;
            h.cy += dy;
        }
        self.navigation_buffer = None;
        if !self.free {
            if let Some((Target::Hint(i), at)) = self.picked {
                if self.hints.get(i).is_some_and(|h| h.scrolls) {
                    self.picked = Some((Target::Hint(i), (at.0 + dx, at.1 + dy)));
                }
            }
        }
    }

    /// Read the window again, now that the scrolling has stopped.
    fn resettle(&mut self) {
        let win = match self.stage {
            Stage::PickHint { win } | Stage::PickTile { win } => win,
            Stage::PickWindow => return,
        };
        let at = self
            .pointer
            .or_else(|| self.picked.map(|(_, at)| at))
            .or_else(hypr::cursor_pos);
        let keep_free = self.free;
        self.settle = None;
        self.load_elements(win, true);
        if self.elements_cache.get(&win).is_none_or(Vec::is_empty) {
            self.hints.clear();
            self.watch = None;
            self.stage = Stage::PickTile { win };
        } else {
            self.install_hint_stage(win);
        }

        self.typed.clear();
        self.armed = None;
        self.aim = None;
        self.teleport = None;
        self.stop_motion = false;
        self.navigation_buffer = None;
        self.lit = None;
        self.picked = None;
        self.pointer = at;
        self.free = keep_free;
        if let Some(at) = at {
            if keep_free {
                self.follow_pointer(at);
                self.show_pointer_indicator(true);
                self.update_anchor_indicator(at);
                self.move_pointer_indicator(at);
            } else if let Some((what, anchor, _)) = self.nearest(at) {
                self.picked = Some((what, anchor));
                self.lit = Some(what);
                self.show_pointer_indicator(false);
                self.show_anchor_indicator(false);
            }
        } else {
            self.show_pointer_indicator(false);
            self.show_anchor_indicator(false);
        }
        self.dirty = true;

        let deferred = std::mem::take(&mut self.deferred_letters);
        for ch in deferred {
            self.press(Key::Char(ch));
            if self.exit {
                break;
            }
        }
    }

    /// A click key coming up releases the matching virtual mouse button.
    fn release(&mut self, key: Key) {
        if let Some(button) = Self::click_button(key) {
            if self.buttons_enabled {
                self.buttons.release(button);
            }
            return;
        }
        match key {
            Key::Arrow(_, NavMode::Instant) => (),
            Key::Arrow(way, mode) => self.hold_arrow(way.index(), mode, false),
            Key::ShiftScroll(_) => self.shift_scroll_held = false,
            _ => {}
        }
    }

    fn press(&mut self, key: Key) {
        if let Some(button) = Self::click_button(key) {
            if self.buttons_enabled && self.buttons.press(button) {
                self.fresh = false;
            }
            return;
        }
        if !key.is_motion_arrow() {
            // Any deliberate non-navigation input ends the chord. This also
            // recovers from a compositor release event that never arrived.
            self.arrows.clear();
            self.stop_motion = true;
        }
        let ch = match key {
            Key::Escape => return self.cancel(),
            Key::Backspace => return self.undo(),
            Key::Tab => return self.pick_window(),
            Key::Reset => return self.reset_or_quit(),
            Key::Switch => return self.switch_mode(),
            Key::Arrow(way, NavMode::Instant) => return self.jump_anchor(way),
            Key::Arrow(way, mode) => return self.hold_arrow(way.index(), mode, true),
            Key::ShiftScroll(w) => {
                if !self.shift_scroll_held {
                    self.shift_scroll_held = true;
                    self.scroll_needs_unshift = true;
                }
                return self.roll(w);
            }
            Key::Scroll(w) => return self.roll(w),
            Key::Char(ch) => ch.to_ascii_lowercase(),
            // Taken above, before navigation state can be changed.
            Key::LeftClick | Key::RightClick => return,
        };
        self.hint_display.typed(ch);
        if ch.is_ascii_alphabetic() {
            self.navigating = None;
            self.dirty = true;
            if self.settle.is_some() {
                self.deferred_letters.push_back(ch);
                return;
            }
        }
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
        self.navigating = None;
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

    /// A hint or tile is complete. An instant selection clicks unless a mouse
    /// button is already down, in which case moving there continues the drag.
    fn pick(&mut self, what: Target, at: (f64, f64)) {
        if config::get().keys.instant && !self.buttons.any() {
            self.target = Some(at);
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
        self.free = false;
        self.show_pointer_indicator(false);
        self.show_anchor_indicator(false);
        self.arrows.clear();
        self.stop_motion = false;
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
        self.boxed()
            .into_iter()
            .map(|(what, at, _)| (what, at))
            .collect()
    }

    /// Targets with their visible bounds. Instant directional movement uses
    /// the bounds so rows and columns behave like rows and columns even when
    /// labels have different widths.
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

    /// Move immediately to the next anchor in the requested direction.
    fn jump_anchor(&mut self, way: Arrow) {
        if matches!(self.stage, Stage::PickWindow) || self.settle.is_some() {
            return;
        }
        let Some(from) = self
            .pointer
            .or_else(|| self.picked.map(|(_, at)| at))
            .or_else(hypr::cursor_pos)
        else {
            return;
        };
        let current = self
            .nearest(from)
            .filter(|(_, _, away)| *away <= CAUGHT)
            .map(|(what, _, _)| what);
        let all = self.boxed();
        let mine = current
            .and_then(|what| {
                all.iter()
                    .find(|(candidate, _, _)| *candidate == what)
                    .map(|(_, _, rect)| *rect)
            })
            .unwrap_or_else(|| Rect::new(from.0 as f32, from.1 as f32, 1.0, 1.0));
        let candidates: Vec<(Target, (f64, f64), Rect)> = all
            .into_iter()
            .filter(|(what, _, _)| Some(*what) != current)
            .collect();
        let boxes: Vec<Rect> = candidates.iter().map(|(_, _, rect)| *rect).collect();
        let next = pull(mine, way.vector(), &boxes, f64::INFINITY, true);
        let now = Instant::now();
        self.navigating = Some(now);
        if !self.show_navigation_frame() {
            self.dirty = true;
        }
        let Some(next) = next else {
            // With no further anchor, the same intent continues into the
            // document instead of making the directional key inert.
            self.queue_edge_scroll(way);
            return;
        };
        let (what, at, _) = candidates[next];
        self.fresh = false;
        self.typed.clear();
        self.armed = None;
        self.aim = None;
        self.teleport = Some((what, at));
        self.stop_motion = true;
    }

    /// An arrow key going down or coming up. Holding one pushes the pointer
    /// that way for as long as it is held, so a tap nudges it a short
    /// distance and a hold carries it across the window.
    fn hold_arrow(&mut self, which: usize, mode: NavMode, down: bool) {
        let now = Instant::now();
        if !self.arrows.set(which, mode, down, now) {
            return;
        }
        if !down {
            return;
        }
        self.fresh = false;
        self.free = true;
        self.aim = None;
        self.stop_motion = false;
        self.navigating = Some(now);
        self.typed.clear();
        self.armed = None;
        if !self.show_navigation_frame() {
            self.dirty = true;
        }
    }

    /// Expire a direction whose release got lost, then combine every key
    /// still down into one unit vector. Normalizing keeps diagonal and
    /// cardinal navigation at the same speed.
    fn motion(&mut self) -> ((f64, f64), NavMode) {
        self.arrows.motion(Instant::now())
    }

    fn any_arrow(&self) -> bool {
        self.arrows.any()
    }

    /// Whether the compact anchor-only frame is active. Pointer navigation
    /// uses it briefly; scrolling keeps it until alphabetic input arrives.
    fn dots(&self) -> bool {
        matches!(self.stage, Stage::PickHint { .. })
            && (self.hint_display.shows_scroll_anchors()
                || self
                    .navigating
                    .is_some_and(|t| t.elapsed() < config::get().label.wake()))
    }

    /// Whatever the pointer is nearest, and how far off it is.
    fn nearest(&self, at: (f64, f64)) -> Option<(Target, (f64, f64), f64)> {
        self.targets()
            .into_iter()
            .map(|(what, p)| (what, p, (p.0 - at.0).hypot(p.1 - at.1)))
            .min_by(|a, b| a.2.total_cmp(&b.2))
    }

    /// Follow whatever the pointer is nearest now without forcing a full
    /// overlay redraw. The small pointer subsurface provides the live visual.
    fn follow_pointer(&mut self, at: (f64, f64)) -> bool {
        let Some((what, _, _)) = self.nearest(at) else {
            return false;
        };
        if self.free {
            // The anchor is highlighted as a landing aid, but the click stays
            // at the freely moved pointer until the anchor actually catches
            // it.
            self.picked = Some((what, at));
        }
        if Some(what) == self.lit {
            return false;
        }
        if std::env::var_os("WL_KEYS").is_some() {
            eprintln!("    lit -> {what:?} at ({:.0},{:.0})", at.0, at.1);
        }
        self.lit = Some(what);
        true
    }

    /// Update the nearest target and make that state visible on the next
    /// full overlay frame.
    fn light_up(&mut self, at: (f64, f64)) {
        if self.follow_pointer(at) {
            self.dirty = true;
        }
    }

    /// Finish a magnetic catch exactly on its anchor.
    fn land(&mut self, at: (f64, f64)) {
        if let Some((what, where_, _)) = self.nearest(at) {
            self.land_target(what, where_);
            return;
        }
        self.pointer = Some(at);
        self.free = false;
        self.show_pointer_indicator(false);
        self.show_anchor_indicator(false);
        self.dirty = true;
    }

    fn land_target(&mut self, what: Target, at: (f64, f64)) {
        self.picked = Some((what, at));
        self.lit = Some(what);
        self.pointer = Some(at);
        self.free = false;
        self.show_pointer_indicator(false);
        self.show_anchor_indicator(false);
        self.dirty = true;
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
        self.arrows.clear();
        self.aim = None;
        self.stop_motion = true;
        self.free = false;
        self.show_pointer_indicator(false);
        self.show_anchor_indicator(false);
        self.navigating = None;
        self.hint_display = HintDisplay::default();
        self.deferred_letters.clear();
        self.navigation_buffer = None;
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
        self.load_elements(win, false);
        if self.elements_cache.get(&win).is_none_or(Vec::is_empty) {
            self.hints.clear();
            self.watch = None;
            self.stage = Stage::PickTile { win };
            self.reset_input();
        } else {
            self.enter_hint_stage(win);
        }
        self.focus_nearest_cursor();
    }

    fn load_elements(&mut self, win: usize, refresh: bool) {
        if refresh {
            self.elements_cache.remove(&win);
        }
        if self.elements_cache.contains_key(&win) {
            return;
        }
        let query = match self.query.take() {
            Some(q) if q.win == win => q,
            other => {
                self.query = other;
                Query::start(&self.snap, win)
            }
        };
        self.elements_cache.insert(win, query.take());
    }

    fn enter_hint_stage(&mut self, win: usize) {
        self.install_hint_stage(win);
        self.reset_input();
    }

    fn install_hint_stage(&mut self, win: usize) {
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
    }

    /// Replace the label frame with the already-rendered anchor frame. The
    /// only work on the input edge is a buffer attach and a surface commit.
    fn show_navigation_frame(&mut self) -> bool {
        if self.showed_dots {
            return true;
        }
        let scale = self.buffer_scale;
        let bw = self.width as i32 * scale;
        let bh = self.height as i32 * scale;
        let usable = self
            .navigation_buffer
            .as_ref()
            .is_some_and(|buffer| buffer.height() == bh && buffer.stride() == bw * 4);
        if !usable {
            self.navigation_buffer = None;
            return false;
        }
        let attached = self
            .navigation_buffer
            .as_ref()
            .is_some_and(|buffer| self.attach_frame(buffer));
        if !attached {
            self.navigation_buffer = None;
            return false;
        }
        self.showed_dots = true;
        self.dirty = false;
        true
    }

    fn draw(&mut self) {
        self.dirty = false;
        let dots = self.dots();
        let previously_showed_dots = self.showed_dots;
        let Some(buffer) = self.render_frame(dots) else {
            return;
        };
        if !self.attach_frame(&buffer) {
            return;
        }
        self.showed_dots = dots;

        // Keep an unused compact frame across normal redraws. If it was just
        // on screen, replace it: the old slot may remain active until the
        // compositor sees this commit.
        if dots {
            self.navigation_buffer = None;
        } else if matches!(self.stage, Stage::PickHint { .. })
            && (previously_showed_dots || self.navigation_buffer.is_none())
        {
            self.navigation_buffer = None;
            self.navigation_buffer = self.render_frame(true);
        }
    }

    /// Paint one complete parent frame without attaching it. Normal draws
    /// attach the label frame and retain the compact frame for the next arrow
    /// press.
    fn render_frame(&mut self, dots: bool) -> Option<Buffer> {
        let scale = self.buffer_scale;
        let bw = self.width as i32 * scale;
        let bh = self.height as i32 * scale;
        if bw <= 0 || bh <= 0 {
            return None;
        }
        let (buffer, buf) = match self
            .pool
            .create_buffer(bw, bh, bw * 4, wl_shm::Format::Argb8888)
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!("buffer allocation failed: {e}");
                return None;
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
                let pointer = self
                    .pointer_overlay
                    .is_none()
                    .then_some(self.pointer)
                    .flatten()
                    .filter(|_| self.free);
                draw_pick_tile(
                    &self.snap,
                    win,
                    lit,
                    pointer,
                    &self.font,
                    &mut canvas,
                    scale,
                )
            }
            Stage::PickHint { win } => {
                let view = HintView {
                    hints: &self.hints,
                    typed: &self.typed,
                    armed: self.armed,
                    // The moving ring and blue nearest-anchor subsurface are
                    // live. Keeping every compact anchor red makes this
                    // cached frame independent of selection changes.
                    picked: if dots {
                        None
                    } else {
                        match self.lit {
                            Some(Target::Hint(i)) => Some(i),
                            _ => None,
                        }
                    },
                    stale: self.settle.is_some(),
                    dots,
                    pointer: self
                        .pointer_overlay
                        .is_none()
                        .then_some(self.pointer)
                        .flatten()
                        .filter(|_| self.free),
                };
                draw_pick_hint(&self.snap, win, view, &self.font, &mut canvas, scale)
            }
        }
        Some(buffer)
    }

    fn attach_frame(&self, buffer: &Buffer) -> bool {
        let Some(layer) = self.layer.as_ref() else {
            return false;
        };
        let scale = self.buffer_scale;
        let bw = self.width as i32 * scale;
        let bh = self.height as i32 * scale;
        let surface = layer.wl_surface();
        surface.set_buffer_scale(scale);
        if buffer.attach_to(surface).is_err() {
            eprintln!("failed to attach buffer");
            return false;
        }
        surface.damage_buffer(0, 0, bw, bh);
        surface.commit();
        true
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
    let els = atspi::clickable_elements(w.pid, &w.title, (w.w, w.h)).unwrap_or_else(|e| {
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
    if els.is_empty() {
        draw_pick_tile(snap, win, armed, None, &font, &mut canvas, scale);
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
        pointer: hints
            .get(hints.len() / 3)
            .map(|h| (h.cx + 40.0, h.cy + 12.0)),
    };
    draw_pick_hint(snap, win, view, &font, &mut canvas, scale);
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
    pointer: Option<(f64, f64)>,
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
    if let Some(at) = pointer {
        draw_pointer(snap, at, canvas, scale);
    }
}

/// The freely moving focus shown during arrow input and magnetic settling.
fn draw_pointer(snap: &Snapshot, (px, py): (f64, f64), canvas: &mut Canvas, scale: i32) {
    let mon = &snap.monitor;
    let cx = ((px - mon.x as f64) * scale as f64) as f32;
    let cy = ((py - mon.y as f64) * scale as f64) as f32;
    draw_pointer_mark(canvas, cx, cy, scale);
}

fn draw_pointer_mark(canvas: &mut Canvas, cx: f32, cy: f32, scale: i32) {
    let s = scale as f32;
    let d = 13.0 * s;
    let spot = Rect::new(cx - d / 2.0, cy - d / 2.0, d, d);
    let glow = *config::get().colors.ring;
    canvas.round_rect_shadow(spot.grow(2.0 * s), d, 9.0 * s, glow.fade(0.42));
    canvas.round_rect_outline(spot, d / 2.0, 2.0 * s, glow);
}

fn draw_nearest_anchor_mark(canvas: &mut Canvas, cx: f32, cy: f32, scale: i32) {
    let s = scale as f32;
    let d = 9.0 * s;
    let spot = Rect::new(cx - d / 2.0, cy - d / 2.0, d, d);
    let colour = *config::get().colors.nearest_dot;
    canvas.round_rect_shadow(
        spot,
        d / 2.0,
        3.0 * s,
        shade(*config::get().colors.shadow, 0.8),
    );
    canvas.round_rect(spot, d / 2.0, colour);
    canvas.round_rect_shadow(spot.grow(2.0 * s), d, 7.0 * s, colour.fade(0.5));
}

/// Where a label sits on screen: buffer pixels, already clamped to the
/// canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// Whether centering a label would hide most of an element or sit directly on
/// a compact icon. An icon hitbox can be somewhat larger than its visible
/// glyph, so area coverage alone is not enough to recognize it.
fn label_hides_target(element: LabelBox, size: (i32, i32)) -> bool {
    let element_w = i64::from(element.w.max(0));
    let element_h = i64::from(element.h.max(0));
    let label_w = i64::from(size.0.max(0));
    let label_h = i64::from(size.1.max(0));
    let area = element_w * element_h;
    if area == 0 {
        return true;
    }
    let covered = element_w.min(label_w) * element_h.min(label_h);
    let mostly_hidden = covered * 5 >= area * 3;
    let compact_icon = (element_w <= label_w * 3 && element_h <= label_h * 2)
        || (element_w <= label_w * 2 && element_h <= label_h * 3);
    mostly_hidden || compact_icon
}

/// Keep a label as close to its element center as possible. The search uses
/// half-label steps so a small collision moves a label only as far as needed.
/// A small target can be kept clear, in which case exact positions beside its
/// four sides and corners join the search. Other compact targets are protected
/// as well, so moving one label off its own element cannot hide a neighbor. If
/// the nearby area is completely crowded, the fallback minimizes target
/// overlap before label overlap and distance.
fn place_label_near(
    center: (i32, i32),
    size: (i32, i32),
    taken: &[LabelBox],
    canvas: (i32, i32),
    gap: i32,
    keep_clear: Option<LabelBox>,
    avoid: &[LabelBox],
) -> LabelBox {
    const SEARCH_RINGS: i32 = 8;

    let (bw, bh) = size;
    let origin = (center.0 - bw / 2, center.1 - bh / 2);
    let fit = |(x, y): (i32, i32)| LabelBox {
        x: x.clamp(0, (canvas.0 - bw).max(0)),
        y: y.clamp(0, (canvas.1 - bh).max(0)),
        w: bw,
        h: bh,
    };
    let step = (((bw + gap + 1) / 2).max(1), ((bh + gap + 1) / 2).max(1));
    let mut candidates = Vec::with_capacity(((SEARCH_RINGS * 2 + 1).pow(2)) as usize);
    for gy in -SEARCH_RINGS..=SEARCH_RINGS {
        for gx in -SEARCH_RINGS..=SEARCH_RINGS {
            candidates.push(fit((origin.0 + gx * step.0, origin.1 + gy * step.1)));
        }
    }
    if let Some(element) = keep_clear {
        let centered_x = element.x + (element.w - bw) / 2;
        let centered_y = element.y + (element.h - bh) / 2;
        let left = element.x - bw - gap;
        let right = element.x + element.w + gap;
        let above = element.y - bh - gap;
        let below = element.y + element.h + gap;
        candidates.extend([
            fit((centered_x, above)),
            fit((centered_x, below)),
            fit((left, centered_y)),
            fit((right, centered_y)),
            fit((left, above)),
            fit((right, above)),
            fit((left, below)),
            fit((right, below)),
        ]);
    }
    // Clamping creates duplicate candidates near screen edges. Remove them
    // before measuring distance and checking collisions.
    candidates.sort_unstable_by_key(|b| (b.x, b.y));
    candidates.dedup_by_key(|b| (b.x, b.y));
    let distance = |b: &LabelBox| {
        let dx = (b.x + b.w / 2 - center.0) as i64;
        let dy = (b.y + b.h / 2 - center.1) as i64;
        dx * dx + dy * dy
    };
    candidates.sort_by_key(&distance);
    let protected = || keep_clear.iter().chain(avoid.iter());
    let clears_targets =
        |candidate: &LabelBox| protected().all(|element| !element.crowds(candidate, gap));

    if let Some(clear) = candidates.iter().find(|candidate| {
        clears_targets(candidate) && !taken.iter().any(|label| label.crowds(candidate, gap))
    }) {
        return *clear;
    }

    let label_score = |candidate: &LabelBox| {
        let overlap: i64 = taken
            .iter()
            .map(|label| candidate.covers((label.x, label.y, label.w, label.h)))
            .sum();
        let crowded = taken
            .iter()
            .filter(|label| label.crowds(candidate, gap))
            .count();
        (overlap, crowded, distance(candidate))
    };
    if let Some(best) = candidates
        .iter()
        .filter(|candidate| clears_targets(candidate))
        .min_by_key(|candidate| label_score(candidate))
    {
        return *best;
    }

    candidates
        .into_iter()
        .min_by_key(|candidate| {
            let target_overlap: i64 = protected()
                .map(|element| candidate.covers((element.x, element.y, element.w, element.h)))
                .sum();
            let target_crowded = protected()
                .filter(|element| element.crowds(candidate, gap))
                .count();
            (target_overlap, target_crowded, label_score(candidate))
        })
        .unwrap_or_else(|| fit(origin))
}

/// Lay out one box per hint.
///
/// Labels start at the element center unless that would obscure a small or
/// compact target, then move the shortest available distance needed to clear
/// every compact target and labels already placed. Placement covers every
/// hint, not just the visible ones, so labels stay put while a prefix is typed.
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
    let gap = ((cfg.gap * scale as f32) as i32).max(0);
    let rect_of = |h: &Hint| {
        (
            ((h.rx - mon.0 as f64) * scale as f64) as i32,
            ((h.ry - mon.1 as f64) * scale as f64) as i32,
            (h.rw * scale as f64) as i32,
            (h.rh * scale as f64) as i32,
        )
    };
    let elements: Vec<LabelBox> = hints
        .iter()
        .map(|hint| {
            let (x, y, w, h) = rect_of(hint);
            LabelBox { x, y, w, h }
        })
        .collect();
    let mut taken: Vec<LabelBox> = Vec::with_capacity(hints.len());
    for (index, h) in hints.iter().enumerate() {
        let element = elements[index];
        let bh = px as i32 + 2 * pad_y;
        // A one-key label keeps its box square rather than turning into a
        // sliver.
        let bw = (label_width(font, &h.label.to_ascii_uppercase(), px, scale as f32) as i32
            + 2 * pad_x)
            .max(bh);
        let center = (element.x + element.w / 2, element.y + element.h / 2);
        let keep_clear = label_hides_target(element, (bw, bh)).then_some(element);
        let avoid: Vec<LabelBox> = elements
            .iter()
            .enumerate()
            .filter(|(other, target)| *other != index && label_hides_target(**target, (bw, bh)))
            .map(|(_, target)| *target)
            .collect();
        let placed = place_label_near(center, (bw, bh), &taken, canvas, gap, keep_clear, &avoid);
        taken.push(placed);
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
    /// The compact fallback replaces labels with one dot per anchor.
    dots: bool,
    /// Where the pointer is, when it is worth drawing: on its way somewhere.
    pointer: Option<(f64, f64)>,
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
        pointer,
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
    // The outline around the window is a long perimeter to measure, and it
    // says which window is being worked on. The compact view omits it because
    // the moving focus ring already supplies the live location.
    if !dots {
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
    }
    // Moving with the arrows: a dot on each target and nothing else, so
    // that the one being moved between them is the thing on screen.
    if dots {
        for (i, h) in hints.iter().enumerate() {
            let hot = picked == Some(i);
            let d = (if hot { 9.0 } else { 6.0 }) * s;
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
                true => *config::get().colors.nearest_dot,
                false => *config::get().colors.dot,
            };
            canvas.round_rect(spot, d / 2.0, colour);
            if hot {
                canvas.round_rect_shadow(spot.grow(2.0 * s), d, 7.0 * s, colour.fade(0.5));
            }
        }
        if let Some(at) = pointer {
            draw_pointer(snap, at, canvas, scale);
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
    if let Some(at) = pointer {
        draw_pointer(snap, at, canvas, scale);
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

fn arrow_of(keysym: Keysym) -> Option<Arrow> {
    match keysym {
        Keysym::Left => Some(Arrow::Left),
        Keysym::Right => Some(Arrow::Right),
        Keysym::Up => Some(Arrow::Up),
        Keysym::Down => Some(Arrow::Down),
        _ => None,
    }
}

fn modified_arrow(way: Arrow, modifiers: Modifiers) -> Key {
    if modifiers.ctrl {
        Key::Arrow(way, NavMode::Instant)
    } else if modifiers.alt {
        Key::Arrow(way, NavMode::Free)
    } else if modifiers.shift {
        Key::ShiftScroll(way.wheel())
    } else {
        Key::Arrow(way, NavMode::Normal)
    }
}

/// The key a wl_keyboard event stands for. Only used on the fallback path,
/// where the overlay holds the keyboard itself.
fn key_of(event: &KeyEvent, modifiers: Modifiers) -> Option<Key> {
    let cfg = &config::get().keys;
    if let Some(way) = arrow_of(event.keysym) {
        return Some(modified_arrow(way, modifiers));
    }
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
    if cfg.scroll_up().is_some_and(|k| k == name) {
        return Some(Key::Scroll(Arrow::Up.wheel()));
    }
    if cfg.scroll_down().is_some_and(|k| k == name) {
        return Some(Key::Scroll(Arrow::Down.wheel()));
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
                state.trace_key("down", *key);
                state.press(*key)
            }
            hyprland_global_shortcut_v1::Event::Released { .. } => {
                state.trace_key("up", *key);
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
        if let Some(key) = key_of(&event, self.keyboard_modifiers) {
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
        if let Some(way) = arrow_of(event.keysym) {
            self.arrows.release_any(way.index(), Instant::now());
            self.shift_scroll_held = false;
        } else if let Some(key) = key_of(&event, self.keyboard_modifiers) {
            self.release(key);
        }
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
        self.keyboard_modifiers = modifiers;
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
delegate_subcompositor!(App);
wayland_client::delegate_noop!(App: ignore wl_region::WlRegion);
wayland_client::delegate_noop!(App: ignore HyprlandGlobalShortcutsManagerV1);
wayland_client::delegate_noop!(App: ignore ZwlrVirtualPointerManagerV1);
wayland_client::delegate_noop!(App: ignore ZwlrVirtualPointerV1);

#[cfg(test)]
mod tests {
    use super::{
        arrow_direction, config, constrain_to_window, label_hides_target, modified_arrow,
        place_label_near, pull, Arrow, ArrowKeys, ButtonEdge, ButtonInput, Drift, Drill,
        HintDisplay, Key, LabelBox, Modifiers, NavMode, Rect, BTN_LEFT, BTN_RIGHT,
    };
    use std::time::{Duration, Instant};

    fn rate() -> f64 {
        config::get().pointer.spring()
    }

    const RIGHT: (f64, f64) = (1.0, 0.0);
    const NONE: (f64, f64) = (0.0, 0.0);
    /// Somewhere for the pointer to run out of, well past anywhere these
    /// tests send it.
    const WALL: (f64, f64) = (10_000.0, 10_000.0);

    #[test]
    fn scroll_anchors_wait_for_alphabetic_input() {
        let mut display = HintDisplay::default();
        display.scrolled();
        assert!(display.shows_scroll_anchors());

        for ch in ['1', ';', '\''] {
            display.typed(ch);
            assert!(display.shows_scroll_anchors());
        }

        display.typed('A');
        assert!(!display.shows_scroll_anchors());
    }

    /// Pull toward a named target and watch the pointer get there: how long
    /// until it has covered all but a twentieth of the way, how long until it
    /// settles, and how far past the target it went.
    fn travel(from: (f64, f64), to: (f64, f64)) -> (u32, u32, f64) {
        let mut d = Drift::toward(from, to, rate());
        let span = to.0 - from.0;
        let (mut ticks, mut mostly, mut furthest) = (0u32, 0u32, 0.0f64);
        for _ in 0..400 {
            ticks += 1;
            let done = d.advance(0.008, NONE, &[], WALL);
            if mostly == 0 && (d.at.0 - from.0).abs() >= span.abs() * 0.95 {
                mostly = ticks;
            }
            furthest = furthest.max(d.at.0 - to.0);
            if done {
                break;
            }
        }
        (mostly * 8, ticks * 8, furthest)
    }

    #[test]
    fn two_arrows_are_one_direction_without_extra_speed() {
        let diagonal = arrow_direction([false, true, false, true]);
        assert!((diagonal.0 - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-9);
        assert!((diagonal.1 - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-9);
        assert!((diagonal.0.hypot(diagonal.1) - 1.0).abs() < 1e-9);
        assert_eq!(
            arrow_direction([true, true, true, true]),
            NONE,
            "opposite keys did not cancel"
        );
        assert_eq!(
            arrow_direction([false; 4]),
            NONE,
            "the chord kept moving after every key was released"
        );
    }

    #[test]
    fn a_partial_chord_release_keeps_the_other_axis() {
        let start = Instant::now();
        let mut arrows = ArrowKeys::default();
        arrows.set(1, NavMode::Normal, true, start);
        arrows.set(3, NavMode::Normal, true, start);
        assert_ne!(arrows.motion(start).0, NONE);

        arrows.set(1, NavMode::Normal, false, start + Duration::from_millis(10));
        assert_eq!(
            arrows.motion(start + Duration::from_millis(20)).0,
            (0.0, 1.0),
            "releasing right did not leave down held"
        );
        assert!(arrows.any(), "the remaining held key was cleared");
        arrows.set(3, NavMode::Normal, false, start + Duration::from_millis(25));
        assert_eq!(
            arrows.motion(start + Duration::from_millis(100)).0,
            NONE,
            "the chord kept pushing after both releases"
        );
    }

    #[test]
    fn a_chord_expires_when_every_release_event_is_lost() {
        let start = Instant::now();
        let mut arrows = ArrowKeys::default();
        arrows.set(1, NavMode::Normal, true, start);
        arrows.set(3, NavMode::Normal, true, start + Duration::from_millis(10));

        // Hyprland repeats only the last key in a chord. That pulse keeps the
        // whole direction alive while the physical chord is still held.
        arrows.set(3, NavMode::Normal, true, start + Duration::from_millis(600));
        assert_ne!(arrows.motion(start + Duration::from_millis(700)).0, NONE);

        // Releasing either physical key stops Hyprland's repeat timer even
        // if neither global-shortcut release reaches the client.
        assert_eq!(
            arrows.motion(start + Duration::from_millis(721)).0,
            NONE,
            "repeat pulses stopped but the chord kept pushing"
        );
        assert!(!arrows.any());
    }

    #[test]
    fn a_drill_can_reproduce_an_arrow_chord() {
        let drill = Drill::parse("down+right:250 wait:40").unwrap();
        assert_eq!(
            drill.steps[0].0,
            vec![
                Key::Arrow(Arrow::Down, NavMode::Normal),
                Key::Arrow(Arrow::Right, NavMode::Normal),
            ]
        );
        assert!(drill.steps[1].0.is_empty());
    }

    #[test]
    fn a_drill_can_select_each_directional_action() {
        let drill = Drill::parse("shift-right free-down instant-left").unwrap();
        assert_eq!(
            drill
                .steps
                .into_iter()
                .map(|(keys, _)| keys[0])
                .collect::<Vec<_>>(),
            vec![
                Key::ShiftScroll(Arrow::Right.wheel()),
                Key::Arrow(Arrow::Down, NavMode::Free),
                Key::Arrow(Arrow::Left, NavMode::Instant),
            ]
        );
    }

    #[test]
    fn a_late_release_cannot_cancel_a_new_modifier_mode() {
        let start = Instant::now();
        let mut arrows = ArrowKeys::default();
        arrows.set(1, NavMode::Normal, true, start);
        arrows.set(1, NavMode::Free, true, start + Duration::from_millis(10));
        arrows.set(1, NavMode::Normal, false, start + Duration::from_millis(20));
        assert_eq!(
            arrows.motion(start + Duration::from_millis(30)),
            (RIGHT, NavMode::Free)
        );
    }

    #[test]
    fn fallback_modifier_precedence_matches_the_shortcuts() {
        assert_eq!(
            modified_arrow(
                Arrow::Left,
                Modifiers {
                    shift: true,
                    ..Modifiers::default()
                }
            ),
            Key::ShiftScroll(Arrow::Left.wheel())
        );
        assert_eq!(
            modified_arrow(
                Arrow::Left,
                Modifiers {
                    shift: true,
                    alt: true,
                    ..Modifiers::default()
                }
            ),
            Key::Arrow(Arrow::Left, NavMode::Free)
        );
        assert_eq!(
            modified_arrow(
                Arrow::Left,
                Modifiers {
                    ctrl: true,
                    alt: true,
                    shift: true,
                    ..Modifiers::default()
                }
            ),
            Key::Arrow(Arrow::Left, NavMode::Instant)
        );
    }

    #[test]
    fn cardinal_and_diagonal_holds_reach_the_same_speed() {
        let diagonal = arrow_direction([false, true, false, true]);
        let mut straight = Drift::free((1000.0, 1000.0));
        let mut corner = Drift::free((1000.0, 1000.0));
        for _ in 0..150 {
            straight.advance(0.008, RIGHT, &[], WALL);
            corner.advance(0.008, diagonal, &[], WALL);
        }
        assert!((straight.speed() - corner.speed()).abs() < 0.01);
    }

    #[test]
    fn a_tap_nudges_and_a_hold_builds_speed() {
        let fly = |push_ticks: usize| {
            let mut d = Drift::free((1000.0, 1000.0));
            for _ in 0..push_ticks {
                d.advance(0.008, RIGHT, &[], WALL);
            }
            let pushed = d.at.0;
            for _ in 0..150 {
                if d.advance(0.008, NONE, &[], WALL) {
                    break;
                }
            }
            (pushed - 1000.0, d.at.0 - 1000.0)
        };
        let (tap, coast) = fly(8);
        assert!((2.0..12.0).contains(&tap), "a tap pushed {tap:.0}px");
        assert!(coast > tap + 5.0, "a tap had no inertia: {coast:.0}px");
        let (released, held) = fly(125);
        assert!(held > 700.0, "a one second hold moved only {held:.0}px");
        let inertia = held - released;
        assert!(
            (20.0..90.0).contains(&inertia),
            "release coast ran {inertia:.0}px"
        );
    }

    #[test]
    fn a_fresh_arrow_starts_below_full_acceleration() {
        let mut fresh = Drift::free((1000.0, 1000.0));
        let mut mature = Drift::free((1000.0, 1000.0));
        let launch = config::get().pointer.launch_speed_px;
        fresh.vel = (launch, 0.0);
        mature.vel = (launch, 0.0);
        mature.push_age = config::get().pointer.ramp_ms as f64 / 1000.0;

        fresh.advance(0.008, RIGHT, &[], WALL);
        mature.advance(0.008, RIGHT, &[], WALL);

        let fraction = (fresh.speed() - launch) / (mature.speed() - launch);
        assert!(
            (0.31..0.34).contains(&fraction),
            "fresh gained {:.0}px/s, mature gained {:.0}px/s ({fraction:.2})",
            fresh.speed() - launch,
            mature.speed() - launch
        );
        fresh.advance(0.008, NONE, &[], WALL);
        assert_eq!(fresh.push_age, 0.0, "release did not reset the ramp");
    }

    #[test]
    fn even_one_motion_frame_gets_a_small_launch_velocity() {
        let mut d = Drift::free((1000.0, 1000.0));
        let launch = config::get().pointer.launch_speed_px;

        d.advance(0.008, RIGHT, &[], WALL);

        assert!(d.speed() >= launch, "launch was only {:.1}px/s", d.speed());
        assert!(
            d.at.0 - 1000.0 < 1.0,
            "minimum launch moved too far for precise control"
        );
    }

    #[test]
    fn an_anchor_bends_a_moving_path() {
        let anchor = (500.0, 500.0);
        let mut attracted = Drift::free((450.0, 470.0));
        let mut plain = Drift::free((450.0, 470.0));
        attracted.vel = (250.0, 0.0);
        plain.vel = attracted.vel;
        for _ in 0..20 {
            attracted.advance(0.008, NONE, &[anchor], WALL);
            plain.advance(0.008, NONE, &[], WALL);
        }
        assert!(
            attracted.at.1 > plain.at.1,
            "the anchor did not bend the moving path"
        );
    }

    #[test]
    fn alt_motion_has_constant_speed_and_stops_where_released() {
        let mut d = Drift::free((450.0, 500.0));
        let anchor = (800.0, 600.0);
        for _ in 0..20 {
            d.advance_as(0.008, RIGHT, NavMode::Free, &[anchor], WALL);
            assert!((d.speed() - config::get().pointer.direct_speed_px).abs() < 0.01);
            assert_eq!(d.at.1, 500.0, "free mode was attracted sideways");
        }
        let released = d.at;
        assert!(d.advance_as(0.008, NONE, NavMode::Normal, &[anchor], WALL));
        assert_eq!(d.at, released);
        assert_eq!(d.vel, NONE);
        assert!(d.to.is_none() && d.magnet.is_none());
    }

    #[test]
    fn instant_selection_prefers_the_next_box_in_a_row() {
        let from = Rect::new(0.0, 0.0, 40.0, 20.0);
        let boxes = [
            Rect::new(300.0, 0.0, 40.0, 20.0),
            Rect::new(80.0, 0.0, 40.0, 20.0),
            Rect::new(80.0, 44.0, 40.0, 20.0),
        ];
        assert_eq!(pull(from, RIGHT, &boxes, f64::INFINITY, true), Some(1));
    }

    #[test]
    fn labels_start_centered_and_move_only_to_avoid_each_other() {
        let first = place_label_near((100, 100), (20, 10), &[], (200, 200), 3, None, &[]);
        assert_eq!(
            first,
            LabelBox {
                x: 90,
                y: 95,
                w: 20,
                h: 10,
            }
        );

        let second = place_label_near((100, 100), (20, 10), &[first], (200, 200), 3, None, &[]);
        assert!(!first.crowds(&second, 3));
        let dx = second.x + second.w / 2 - 100;
        let dy = second.y + second.h / 2 - 100;
        assert!(dx * dx + dy * dy <= 24 * 24, "label moved too far");

        let edge = place_label_near((2, 2), (20, 10), &[], (200, 200), 3, None, &[]);
        assert_eq!((edge.x, edge.y), (0, 0));
    }

    #[test]
    fn labels_sit_beside_small_elements_without_hiding_them() {
        let element = LabelBox {
            x: 92,
            y: 92,
            w: 16,
            h: 16,
        };
        assert!(label_hides_target(element, (20, 10)));
        assert!(!label_hides_target(
            LabelBox {
                w: 80,
                h: 40,
                ..element
            },
            (20, 10)
        ));

        let first = place_label_near((100, 100), (20, 10), &[], (200, 200), 3, Some(element), &[]);
        assert!(!element.crowds(&first, 3));
        let first_dx = first.x + first.w / 2 - 100;
        let first_dy = first.y + first.h / 2 - 100;
        assert!(first_dx * first_dx + first_dy * first_dy <= 21 * 21);

        let second = place_label_near(
            (100, 100),
            (20, 10),
            &[first],
            (200, 200),
            3,
            Some(element),
            &[],
        );
        assert!(!element.crowds(&second, 3));
        assert!(!first.crowds(&second, 3));
        let second_dx = second.x + second.w / 2 - 100;
        let second_dy = second.y + second.h / 2 - 100;
        assert!(second_dx * second_dx + second_dy * second_dy <= 21 * 21);
    }

    #[test]
    fn moving_a_label_off_its_target_does_not_hide_a_neighbor() {
        let element = LabelBox {
            x: 92,
            y: 92,
            w: 16,
            h: 16,
        };
        let first_choice =
            place_label_near((100, 100), (20, 10), &[], (200, 200), 3, Some(element), &[]);
        let neighbor = LabelBox {
            x: first_choice.x,
            y: first_choice.y,
            w: 12,
            h: 12,
        };
        let placed = place_label_near(
            (100, 100),
            (20, 10),
            &[],
            (200, 200),
            3,
            Some(element),
            &[neighbor],
        );

        assert!(!element.crowds(&placed, 3));
        assert!(!neighbor.crowds(&placed, 3));
        assert_ne!(placed, first_choice);
    }

    #[test]
    fn compact_bookmark_hitboxes_keep_labels_off_the_icon() {
        let bookmark = LabelBox {
            x: 75,
            y: 86,
            w: 51,
            h: 28,
        };
        let label_size = (24, 16);
        assert!(label_hides_target(bookmark, label_size));
        assert!(!label_hides_target(
            LabelBox {
                w: 80,
                h: 20,
                ..bookmark
            },
            label_size
        ));

        let placed = place_label_near(
            (100, 100),
            label_size,
            &[],
            (200, 200),
            3,
            Some(bookmark),
            &[],
        );
        assert!(!bookmark.crowds(&placed, 3));
    }

    #[test]
    fn small_elements_at_screen_edges_keep_their_labels_visible() {
        let element = LabelBox {
            x: 0,
            y: 0,
            w: 12,
            h: 12,
        };
        let placed = place_label_near((6, 6), (20, 10), &[], (200, 200), 3, Some(element), &[]);
        assert!(!element.crowds(&placed, 3));
        assert!(placed.x >= 0 && placed.y >= 0);
        assert!(placed.x + placed.w <= 200 && placed.y + placed.h <= 200);
    }

    #[test]
    fn a_dense_label_cluster_spreads_nearby_without_overlap() {
        let mut placed = Vec::new();
        for _ in 0..25 {
            let next = place_label_near((100, 100), (20, 10), &placed, (200, 200), 3, None, &[]);
            assert!(
                placed.iter().all(|label| !label.crowds(&next, 3)),
                "dense placement overlapped an existing label"
            );
            placed.push(next);
        }
        let furthest = placed
            .iter()
            .map(|label| {
                let dx = label.x + label.w / 2 - 100;
                let dy = label.y + label.h / 2 - 100;
                dx * dx + dy * dy
            })
            .max()
            .unwrap();
        assert!(furthest <= 60 * 60, "dense labels spread too far");
    }

    #[test]
    fn click_keys_queue_one_live_button_edge_each() {
        let mut buttons = ButtonInput::default();
        assert!(buttons.press(BTN_LEFT));
        assert!(!buttons.press(BTN_LEFT), "a key repeat pressed twice");
        assert!(buttons.any());
        assert_eq!(
            buttons.next(),
            Some(ButtonEdge {
                button: BTN_LEFT,
                down: true,
            })
        );
        assert_eq!(buttons.next(), None);

        assert!(buttons.release(BTN_LEFT));
        assert!(!buttons.any());
        assert_eq!(
            buttons.next(),
            Some(ButtonEdge {
                button: BTN_LEFT,
                down: false,
            })
        );
        assert!(buttons.take_finish());
        assert!(!buttons.take_finish());
    }

    #[test]
    fn cancelling_releases_every_held_button() {
        let mut buttons = ButtonInput::default();
        buttons.press(BTN_LEFT);
        buttons.press(BTN_RIGHT);
        assert_eq!(buttons.next().map(|edge| edge.down), Some(true));
        assert_eq!(buttons.next().map(|edge| edge.down), Some(true));

        buttons.release_all();
        assert!(!buttons.any());
        assert_eq!(
            [buttons.next(), buttons.next()],
            [
                Some(ButtonEdge {
                    button: BTN_LEFT,
                    down: false,
                }),
                Some(ButtonEdge {
                    button: BTN_RIGHT,
                    down: false,
                }),
            ]
        );
    }

    #[test]
    fn instant_selection_treats_uneven_links_as_one_column() {
        let rows: Vec<Rect> = (0..6)
            .map(|i| {
                Rect::new(
                    645.0,
                    476.0 + i as f32 * 18.0,
                    120.0 + i as f32 * 60.0,
                    16.0,
                )
            })
            .collect();
        for i in 0..5 {
            assert_eq!(
                pull(rows[i], (0.0, 1.0), &rows[i + 1..], f64::INFINITY, true,),
                Some(0),
                "row {i} did not select the link below"
            );
        }
    }

    #[test]
    fn instant_selection_never_jumps_behind_the_requested_direction() {
        let from = Rect::new(600.0, 500.0, 80.0, 16.0);
        let behind = [Rect::new(400.0, 500.0, 80.0, 16.0)];
        assert_eq!(pull(from, RIGHT, &behind, f64::INFINITY, true), None);
    }

    #[test]
    fn outward_motion_clamps_and_reports_window_edges() {
        let mut at = (99.0, 99.0);
        let mut vel = (200.0, 200.0);
        let hit = constrain_to_window(
            &mut at,
            &mut vel,
            arrow_direction([false, true, false, true]),
            (0.0, 0.0, 100.0, 100.0),
        );
        assert_eq!(hit, vec![Arrow::Right, Arrow::Down]);
        assert_eq!(at, (97.0, 97.0));
        assert_eq!(vel, NONE);
    }

    #[test]
    fn a_released_pointer_inside_the_snap_radius_lands_exactly() {
        let anchor = (500.0, 500.0);
        let mut d = Drift::free((493.0, 500.0));
        let mut done = false;
        for _ in 0..400 {
            done = d.advance(0.008, NONE, &[anchor], WALL);
            if done {
                break;
            }
        }
        assert!(done, "the anchor never caught the pointer");
        assert_eq!(d.at, anchor);
        assert_eq!(d.vel, NONE);
    }

    #[test]
    fn released_speed_cannot_skip_the_snap_zone() {
        let anchor = (500.0, 500.0);
        let mut d = Drift::free((493.0, 500.0));
        d.vel = (800.0, 0.0);

        d.advance(0.008, NONE, &[anchor], WALL);

        assert_eq!(d.to, Some(anchor));
        assert!(d.magnet.is_none());
    }

    #[test]
    fn attraction_keeps_pulling_below_the_idle_speed() {
        let anchor = (500.0, 500.0);
        let start = (460.0, 500.0);
        let mut d = Drift::free(start);
        let done = d.advance(0.008, NONE, &[anchor], WALL);
        assert!(!done, "attraction stopped as if the pointer were idle");
        assert_eq!(d.magnet, Some(anchor));
        assert!(d.at.0 > start.0, "attraction did not move the pointer");
        assert!(
            d.to.is_none(),
            "attraction skipped straight to the snap spring"
        );
    }

    #[test]
    fn release_chooses_the_anchor_nearest_the_coast_endpoint() {
        let behind = (500.0, 500.0);
        let ahead = (598.0, 500.0);
        let mut d = Drift::free((522.0, 500.0));
        d.vel = (456.0, 0.0);
        d.guard_departure(behind, RIGHT);

        d.advance(0.008, NONE, &[behind, ahead], WALL);

        assert_eq!(d.magnet, Some(ahead));
        assert!(d.vel.0 > 0.0, "attraction reversed a rightward release");
    }

    #[test]
    fn a_released_pointer_inside_attraction_lands_exactly() {
        let anchor = (500.0, 500.0);
        let mut d = Drift::free((440.0, 500.0));
        let mut done = false;
        for _ in 0..400 {
            done = d.advance(0.008, NONE, &[anchor], WALL);
            if done {
                break;
            }
        }
        assert!(done, "attraction never reached the snap zone");
        assert_eq!(d.at, anchor);
        assert_eq!(d.vel, NONE);
    }

    #[test]
    fn distant_attraction_pulls_like_a_stretched_spring() {
        let anchor = (500.0, 500.0);
        let mut near = Drift::free((460.0, 500.0));
        let mut far = Drift::free((100.0, 500.0));

        near.advance(0.008, NONE, &[anchor], WALL);
        far.advance(0.008, NONE, &[anchor], WALL);

        assert!(
            far.speed() > near.speed() * 6.0,
            "far pull {:.1}px/s was too close to near pull {:.1}px/s",
            far.speed(),
            near.speed()
        );
    }

    #[test]
    fn released_motion_outside_the_softness_radius_still_lands() {
        let anchor = (800.0, 500.0);
        let farther = (1200.0, 500.0);
        let mut d = Drift::free((400.0, 500.0));
        for _ in 0..8 {
            d.advance(0.008, RIGHT, &[farther, anchor], WALL);
        }
        d.advance(0.008, NONE, &[farther, anchor], WALL);
        assert_eq!(d.magnet, Some(anchor), "release chose a farther anchor");
        let mut settle_ticks = 0;
        for _ in 0..600 {
            settle_ticks += 1;
            if d.advance(0.008, NONE, &[farther, anchor], WALL) {
                break;
            }
        }
        assert!(
            settle_ticks * 8 < 1200,
            "distant attraction took {}ms",
            settle_ticks * 8
        );
        assert_eq!(d.at, anchor, "distance disabled attraction");
        assert_eq!(d.vel, NONE);
    }

    #[test]
    fn a_short_release_near_an_anchor_settles_exactly() {
        let anchor = (500.0, 500.0);
        let mut d = Drift::free(anchor);
        d.guard_departure(anchor, RIGHT);
        let mut furthest = d.at.0;
        for _ in 0..8 {
            d.advance(0.008, RIGHT, &[anchor], WALL);
            furthest = furthest.max(d.at.0);
        }
        for _ in 0..400 {
            if d.advance(0.008, NONE, &[anchor], WALL) {
                break;
            }
            furthest = furthest.max(d.at.0);
        }
        assert!(
            furthest > anchor.0 + 2.0,
            "the arrow did not move before attraction took over"
        );
        assert_eq!(d.at, anchor, "the released pointer floated near its origin");
    }

    #[test]
    fn a_short_tap_cannot_be_recaptured_by_its_departure_anchor() {
        let origin = (500.0, 500.0);
        let adjacent = (514.0, 500.0);
        let mut d = Drift::free(origin);
        d.guard_departure(origin, RIGHT);

        d.advance(0.008, RIGHT, &[origin, adjacent], WALL);
        d.advance(0.008, NONE, &[origin, adjacent], WALL);

        assert_eq!(d.departure.map(|guard| guard.anchor), Some(origin));
        assert_eq!(d.magnet, Some(adjacent));
        assert!(d.to.is_none(), "the departure anchor snapped the short tap");
        for _ in 0..300 {
            if d.advance(0.008, NONE, &[origin, adjacent], WALL) {
                break;
            }
        }
        assert_eq!(d.at, adjacent, "the short tap did not reach its neighbor");
    }

    #[test]
    fn a_short_tap_rejects_a_closer_anchor_behind_its_direction() {
        let behind = (490.0, 500.0);
        let source = (500.0, 500.0);
        let ahead = (900.0, 500.0);
        let mut d = Drift::free(source);
        d.guard_departure(source, RIGHT);

        d.advance(0.008, RIGHT, &[behind, source, ahead], WALL);
        d.advance(0.008, NONE, &[behind, source, ahead], WALL);

        assert_eq!(d.magnet, Some(ahead));
        assert!(d.vel.0 > 0.0, "the closer anchor behind reversed the tap");
    }

    #[test]
    fn a_departure_anchor_returns_after_its_grace_period() {
        let anchor = (500.0, 500.0);
        let mut d = Drift::free(anchor);
        d.guard_departure(anchor, RIGHT);
        let ticks = config::get().pointer.departure_ms / 8 + 2;
        for _ in 0..ticks {
            d.advance(0.008, RIGHT, &[anchor], WALL);
        }
        assert!(
            d.departure.is_none(),
            "directional departure grace did not expire"
        );

        d.at = (anchor.0 + 6.0, anchor.1);
        d.vel = NONE;
        for _ in 0..400 {
            if d.advance(0.008, NONE, &[anchor], WALL) {
                break;
            }
        }
        assert_eq!(d.at, anchor, "returning pointer did not snap home");
    }

    #[test]
    fn a_held_key_can_push_through_the_snap_radius() {
        let anchor = (500.0, 500.0);
        let mut d = Drift::free((450.0, 500.0));
        for _ in 0..50 {
            d.advance(0.008, RIGHT, &[anchor], WALL);
        }
        assert!(d.at.0 > anchor.0, "the anchor trapped a held key");
        assert!(d.to.is_none(), "snapped while an arrow was still down");
    }

    #[test]
    fn arrow_takeover_drops_cancelled_spring_velocity() {
        let target = (500.0, 500.0);
        let mut d = Drift::toward((400.0, 500.0), target, rate());
        d.vel = (-700.0, 250.0);
        d.release_target(RIGHT);
        assert!(d.to.is_none());
        assert_eq!(d.departure.map(|guard| guard.anchor), Some(target));
        assert!(d.departure_for > 0.0);
        assert_eq!(d.vel, NONE);
    }

    #[test]
    fn a_named_target_eases_in_and_out() {
        let (mostly, settled, past) = travel((0.0, 0.0), (300.0, 0.0));
        // Long enough to be a movement rather than a jump, short enough not
        // to be waited on.
        assert!(
            (120..420).contains(&mostly),
            "a step covered itself in {mostly}ms"
        );
        assert!(settled < 700, "and took {settled}ms to settle");
        // A hair past the target and back is the tension in it; a long way
        // past is a pointer to be chased.
        assert!((2.0..40.0).contains(&past), "overshot by {past:.0}px");
    }

    #[test]
    fn motion_stops_at_the_edge_of_the_screen() {
        let mut d = Drift::toward((0.0, 0.0), (9000.0, 0.0), rate());
        for _ in 0..200 {
            d.advance(0.008, NONE, &[], (300.0, 300.0));
        }
        assert_eq!(d.at.0, 300.0, "went past the edge");
        assert_eq!(d.vel.0, 0.0, "still carrying speed into the wall");
    }
}
