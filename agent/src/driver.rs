//! This device driving a peer: the capture backend hands over the pointer
//! at a barrier, this tracks it on the peer with the core's
//! [`RemotePointer`] (so both ends agree where it is), forwards keys and
//! buttons, and gives the pointer back where it leaves the peer.

use rinflow_core::layout::{Motion, RemotePointer};
use rinflow_core::{
    Core, LC_INPUT_FLAG_ABSOLUTE_POSITION, LC_INPUT_FLAG_KEY_REPEAT, LC_INPUT_FLAG_PRECISE_SCROLL,
    LC_INPUT_KEY_DOWN, LC_INPUT_KEY_UP, LC_INPUT_MOUSE_BUTTON_DOWN, LC_INPUT_MOUSE_BUTTON_UP,
    LC_INPUT_MOUSE_MOVE, LC_INPUT_SCROLL, LC_MODIFIER_COMMAND, LC_MODIFIER_CONTROL,
    LC_MODIFIER_LEFT_COMMAND, LC_MODIFIER_LEFT_CONTROL, LC_MODIFIER_LEFT_OPTION,
    LC_MODIFIER_LEFT_SHIFT, LC_MODIFIER_OPTION, LC_MODIFIER_RIGHT_COMMAND,
    LC_MODIFIER_RIGHT_CONTROL, LC_MODIFIER_RIGHT_OPTION, LC_MODIFIER_RIGHT_SHIFT,
    LC_MODIFIER_SHIFT, LcInputEvent,
};
use rinflow_shell::arrangement::{Arrangement, EdgePortal};
use rinflow_shell::settings::{PeerInput, SettingsStore};
use rinflow_shell::state::SharedState;
use rinflow_input::{Barrier, CaptureCommand, CaptureEvent};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Keys repeat after this long, then at this interval, as on the device
/// itself; the compositor does not repeat keys it hands to a receiver.
const REPEAT_DELAY: Duration = Duration::from_millis(400);
const REPEAT_INTERVAL: Duration = Duration::from_millis(40);

const USAGE_ESCAPE: u32 = 0x29;
const USAGE_SCROLL_LOCK: u32 = 0x47;
/// While the pointer waits at an edge (`Settings::switch_delay_ms`): it
/// goes back when the user stops pushing for this long, or pulls it this
/// far inward.
const PUSH_PAUSE: Duration = Duration::from_millis(200);
const PULL_BACK: f64 = 6.0;
/// An edge hit this soon after a peer's input was played here is the
/// peer's pointer reaching an edge, not this device's user crossing.
const REMOTE_INPUT_GRACE_MS: u64 = 500;

pub struct Driver {
    core: Weak<Core>,
    state: SharedState,
    arrangement: Arc<Arrangement>,
    settings: Arc<SettingsStore>,
    capture: Option<mpsc::UnboundedSender<CaptureCommand>>,
    /// Barrier `n` is `portals[n - 1]`.
    portals: Vec<EdgePortal>,
    current: Option<Driving>,
    last_remote_input: Arc<AtomicU64>,
    /// An edge hit while a peer drove this device was released; when the
    /// compositor confirms, the peer's entry move is played again.
    replay_after_release: bool,
    /// Keys the capture hands over while nothing is driven. That should not
    /// happen; when it does the pointer is stuck in the capture, and these
    /// let Ctrl+Alt+Esc get it back all the same.
    stray_keys: HashSet<u32>,
    /// The pointer hit an edge and is held there until it has pushed long
    /// enough to cross (`Settings::switch_delay_ms`).
    pending: Option<Pending>,
}

struct Pending {
    edge: EdgePortal,
    x: f64,
    y: f64,
    since: Instant,
    last_push: Instant,
    /// How far the pointer was pulled back since it last pushed.
    inward: f64,
}

struct Driving {
    peer_id: String,
    pointer: RemotePointer,
    keys: HashSet<u32>,
    buttons: u32,
    input: PeerInput,
    /// A game on the peer holds its pointer: motion goes there whole and
    /// no edge leads anywhere.
    pointer_held: bool,
    repeat: Option<(u32, Instant, u32)>,
}

impl Driver {
    pub fn new(
        core: Weak<Core>,
        state: SharedState,
        arrangement: Arc<Arrangement>,
        settings: Arc<SettingsStore>,
        last_remote_input: Arc<AtomicU64>,
    ) -> Self {
        Self {
            core,
            state,
            arrangement,
            settings,
            capture: None,
            portals: Vec::new(),
            current: None,
            last_remote_input,
            replay_after_release: false,
            stray_keys: HashSet::new(),
            pending: None,
        }
    }

    /// Whether the release just confirmed by the compositor needs the
    /// peer's entry move played again (see `activated`).
    pub fn take_replay(&mut self) -> bool {
        std::mem::take(&mut self.replay_after_release)
    }

    /// A peer played input here within the grace period: the pointer is
    /// the peer's right now.
    fn remotely_moved(&self) -> bool {
        let last = self.last_remote_input.load(Ordering::Relaxed);
        if last == 0 {
            return false;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        now.saturating_sub(last) < REMOTE_INPUT_GRACE_MS
    }

    /// Whether `tick` has anything to do: key repeat, or an edge delay.
    pub fn ticking(&self) -> bool {
        self.current.is_some() || self.pending.is_some()
    }

    /// Brings the pointer home from the peer being driven (the window, the
    /// tray or `rinflowctl home`; Ctrl+Alt+Esc does the same).
    pub fn home(&mut self) {
        let Some(driving) = &self.current else { return };
        let (x, y) = driving.pointer.local_position();
        let home = driving.pointer.return_point(x, y);
        self.end(Some(home));
    }

    /// The capture backend to talk to (none while it is down).
    pub fn set_capture(&mut self, capture: Option<mpsc::UnboundedSender<CaptureCommand>>) {
        if capture.is_none() && self.current.is_some() {
            // The compositor took the pointer back with the session.
            self.end(None);
        }
        self.capture = capture;
        self.refresh_barriers();
    }

    /// Barriers from the portals on this device's edges, or none while
    /// sharing is paused.
    pub fn refresh_barriers(&mut self) {
        let Some(capture) = &self.capture else { return };
        // While a peer drives this device its pointer is the peer's; an
        // edge it reaches is for the peer's tracking to handle, not for
        // this device to cross from. And nothing while sharing is paused.
        let controlled = self
            .state
            .snapshot()
            .sessions
            .iter()
            .any(|session| session.remote_control);
        // Locked to this screen: no edge leads anywhere. While a peer is
        // driven the barriers stay as they are (changing them would end
        // the capture); the lock then keeps the pointer on the peer.
        if self.current.is_some() {
            return;
        }
        let locked = self.state.locked();
        self.portals = if self.settings.get().sharing && !controlled && !locked {
            self.arrangement.portals()
        } else {
            Vec::new()
        };
        let barriers = self
            .portals
            .iter()
            .enumerate()
            .map(|(index, edge)| Barrier {
                id: index as u32 + 1,
                side: edge.portal.side,
                line: edge.portal.line,
                from: edge.portal.from,
                to: edge.portal.to,
            })
            .collect();
        let _ = capture.send(CaptureCommand::SetBarriers(barriers));
    }

    /// Picks what concerns the crossing out of the core's events.
    pub fn core_event(&mut self, event: &Value) {
        let Some(peer_id) = event["peer_id"].as_str() else { return };
        let mine = self
            .current
            .as_ref()
            .is_some_and(|driving| driving.peer_id == peer_id);
        if !mine {
            return;
        }
        match event["event"].as_str() {
            Some("peer_pointer_lock") => {
                if let Some(driving) = &mut self.current {
                    driving.pointer_held = event["locked"].as_bool().unwrap_or(false);
                    if !driving.pointer_held {
                        // Pull the peer's cursor back to where it is tracked.
                        let (x, y) = driving.pointer.position();
                        self.send(absolute_move(x, y));
                    }
                }
            }
            // The peer took its input back, or went away: the pointer
            // comes home just inside the edge it left through.
            Some("control_revoked" | "disconnected") => {
                let home = self.current.as_ref().map(|driving| {
                    let (x, y) = driving.pointer.local_position();
                    driving.pointer.return_point(x, y)
                });
                self.end(home);
            }
            _ => {}
        }
    }

    pub fn handle_capture(&mut self, event: CaptureEvent) {
        match event {
            CaptureEvent::Activated { barrier, x, y } => self.activated(barrier, x, y),
            CaptureEvent::Motion { dx, dy } => self.motion(dx, dy),
            CaptureEvent::Button { button, down } => self.button(button, down),
            CaptureEvent::Scroll { dx, dy, precise } => self.scroll(dx, dy, precise),
            CaptureEvent::Key { usage, down } => self.key(usage, down),
            CaptureEvent::Deactivated | CaptureEvent::Lost(_) | CaptureEvent::Denied(_) => {
                self.stray_keys.clear();
                self.pending = None;
                if self.current.is_some() {
                    self.end(None);
                }
            }
            CaptureEvent::Ready | CaptureEvent::Prompting | CaptureEvent::Zones(_) => {}
        }
    }

    /// Key repeat; call every few tens of milliseconds while driving.
    pub fn tick(&mut self) {
        let now = Instant::now();
        if let Some(pending) = &self.pending {
            if now.duration_since(pending.last_push) > PUSH_PAUSE {
                // The user stopped pushing: not a crossing.
                log::debug!("edge delay: the pointer stopped pushing, given back");
                let pending = self.pending.take().expect("checked above");
                self.release_at(pending.x, pending.y, Some(pending.edge.portal.side));
            } else if now.duration_since(pending.since) >= Duration::from_millis(u64::from(self.settings.get().switch_delay_ms)) {
                let pending = self.pending.take().expect("checked above");
                self.enter(pending.edge, pending.x, pending.y);
            }
            return;
        }
        let Some(driving) = &mut self.current else { return };
        let Some((usage, due, count)) = driving.repeat else { return };
        if now < due {
            return;
        }
        let count = count.saturating_add(1);
        driving.repeat = Some((usage, due + REPEAT_INTERVAL, count));
        let modifiers = driving.modifiers();
        let buttons = driving.buttons;
        self.send(LcInputEvent {
            kind: LC_INPUT_KEY_DOWN,
            flags: LC_INPUT_FLAG_KEY_REPEAT,
            code: usage,
            repeat_count: count,
            modifiers,
            buttons,
            ..LcInputEvent::default()
        });
    }

    fn activated(&mut self, barrier: u32, x: f64, y: f64) {
        let Some(edge) = self.portals.get(barrier as usize - 1).cloned() else {
            self.release_at(x, y, None);
            return;
        };
        if self.remotely_moved() {
            // The peer driving this device moved its pointer into an edge;
            // its own tracking decides what that means. Not a crossing.
            log::debug!("edge hit while a peer drives this device: released");
            self.release_at(x, y, Some(edge.portal.side));
            self.replay_after_release = true;
            return;
        }
        let settings = self.settings.get();
        if self.in_corner(&edge, x, y, f64::from(settings.corner_guard)) {
            log::debug!("edge hit within the corner margin: released");
            self.release_at(x, y, Some(edge.portal.side));
            return;
        }
        if settings.switch_delay_ms > 0 {
            // Held at the edge; `tick` and `motion` decide (see `Pending`).
            let now = Instant::now();
            log::debug!("edge delay: holding the pointer at the edge for {} ms", settings.switch_delay_ms);
            self.pending = Some(Pending { edge, x, y, since: now, last_push: now, inward: 0.0 });
            return;
        }
        self.enter(edge, x, y);
    }

    /// Whether an edge hit at `(x, y)` is within `guard` points of an end
    /// of the display edge it is on.
    fn in_corner(&self, edge: &EdgePortal, x: f64, y: f64, guard: f64) -> bool {
        if guard <= 0.0 {
            return false;
        }
        let vertical = edge.portal.side.is_vertical();
        let along = if vertical { y } else { x };
        self.arrangement.displays().iter().any(|display| {
            let (start, length, near, far) = if vertical {
                (display.y, display.height, display.x, display.x + display.width)
            } else {
                (display.x, display.width, display.y, display.y + display.height)
            };
            let on_this_edge = (edge.portal.line - near).abs() < 1.5 || (edge.portal.line - far).abs() < 1.5;
            // A display too short for two margins keeps its middle third.
            let guard = guard.min(length / 3.0);
            on_this_edge
                && along >= start
                && along <= start + length
                && (along - start < guard || start + length - along < guard)
        })
    }

    /// Crosses into the peer behind `edge` from `(x, y)` on it, or gives
    /// the pointer straight back when that cannot be done now.
    fn enter(&mut self, edge: EdgePortal, x: f64, y: f64) {
        let peer_id = edge.peer_id;
        if !self.state.is_connected(&peer_id) {
            // Not now: the core connects paired peers by itself; the
            // pointer goes straight back, a step away from the barrier.
            if let Some(core) = self.core.upgrade() {
                let _ = core.connect(&peer_id);
            }
            self.release_at(x, y, Some(edge.portal.side));
            return;
        }
        let Some(target) = self.arrangement.target(&peer_id) else {
            self.release_at(x, y, Some(edge.portal.side));
            return;
        };
        let Some(core) = self.core.upgrade() else {
            self.release_at(x, y, Some(edge.portal.side));
            return;
        };
        // Exactly on the edge.
        let (left_x, left_y) = if edge.portal.side.is_vertical() {
            (edge.portal.line, y)
        } else {
            (x, edge.portal.line)
        };
        let pointer = RemotePointer::enter(
            &target.peer,
            &target.portals,
            target.offset,
            edge.portal,
            left_x,
            left_y,
        )
        .with_neighbors(target.neighbors);
        if core.set_input_target(Some(&peer_id)).is_err() {
            self.release_at(x, y, Some(edge.portal.side));
            return;
        }
        let (arrival_x, arrival_y) = pointer.position();
        log::info!("driving {peer_id} from ({left_x:.0}, {left_y:.0}), arriving at ({arrival_x:.0}, {arrival_y:.0})");
        self.stray_keys.clear();
        self.current = Some(Driving {
            input: self.settings.get().input_for(&peer_id),
            peer_id: peer_id.clone(),
            pointer,
            keys: HashSet::new(),
            buttons: 0,
            pointer_held: false,
            repeat: None,
        });
        self.send(absolute_move(arrival_x, arrival_y));
        self.state.set_driving(Some(peer_id));
    }

    fn motion(&mut self, dx: f64, dy: f64) {
        if let Some(pending) = &mut self.pending {
            use rinflow_core::layout::Side;
            let (outward, vertical) = match pending.edge.portal.side {
                Side::Right => (dx, true),
                Side::Left => (-dx, true),
                Side::Bottom => (dy, false),
                Side::Top => (-dy, false),
            };
            // The entry point follows the pointer along the edge.
            let (from, to) = (pending.edge.portal.from, pending.edge.portal.to - 1.0);
            if vertical {
                pending.y = (pending.y + dy).clamp(from, to.max(from));
            } else {
                pending.x = (pending.x + dx).clamp(from, to.max(from));
            }
            if outward > 0.0 {
                pending.last_push = Instant::now();
                pending.inward = 0.0;
            } else {
                pending.inward -= outward;
                if pending.inward > PULL_BACK {
                    log::debug!("edge delay: the pointer was pulled back, given back");
                    let pending = self.pending.take().expect("checked above");
                    self.release_at(pending.x, pending.y, Some(pending.edge.portal.side));
                }
            }
            return;
        }
        let locked = self.state.locked();
        let Some(driving) = &mut self.current else { return };
        let speed = driving.input.pointer_speed;
        let (dx, dy) = (dx * speed, dy * speed);
        if driving.pointer_held {
            let event = relative_move(dx, dy, driving);
            self.send(event);
            return;
        }
        let mut motion = driving.pointer.step(dx, dy);
        if locked && !matches!(motion, Motion::Stay) {
            // Kept on this peer: the pointer slides along the edge it
            // would have left through (the peer's desktop clamps the same
            // way), and nothing crosses.
            if matches!(motion, Motion::Home(..)) {
                let _ = driving.pointer.step(dx, 0.0);
                let _ = driving.pointer.step(0.0, dy);
            }
            motion = Motion::Stay;
        }
        match motion {
            Motion::Stay => {
                let event = relative_move(dx, dy, driving);
                self.send(event);
            }
            Motion::Home(x, y) => {
                log::info!("back from {} at ({x:.0}, {y:.0})", driving.peer_id);
                self.end(Some((x, y)));
            }
            Motion::Neighbor { id, x, y } => {
                if self.state.is_connected(&id) {
                    self.move_on(id, x, y);
                }
            }
        }
    }

    /// On into the placed peer that touches this one, where the pointer
    /// left it; modifiers held stay held there.
    fn move_on(&mut self, peer_id: String, x: f64, y: f64) {
        let Some(target) = self.arrangement.target(&peer_id) else { return };
        let Some(core) = self.core.upgrade() else { return };
        let Some(driving) = &mut self.current else { return };
        let entry = driving.pointer.entry();
        let pointer = RemotePointer::enter_at(&target.peer, &target.portals, target.offset, x, y, entry)
            .with_neighbors(target.neighbors);
        if core.set_input_target(Some(&peer_id)).is_err() {
            return;
        }
        let held: Vec<u32> = driving.keys.drain().collect();
        driving.buttons = 0;
        driving.repeat = None;
        driving.pointer_held = false;
        driving.peer_id = peer_id.clone();
        driving.pointer = pointer;
        driving.input = self.settings.get().input_for(&peer_id);
        let (arrival_x, arrival_y) = driving.pointer.position();
        let mut events = vec![absolute_move(arrival_x, arrival_y)];
        for usage in held.into_iter().filter(|usage| (0xE0..=0xE7).contains(usage)) {
            driving.keys.insert(usage);
            events.push(LcInputEvent {
                kind: LC_INPUT_KEY_DOWN,
                code: usage,
                modifiers: driving.modifiers(),
                ..LcInputEvent::default()
            });
        }
        log::info!("moved on to {peer_id}, arriving at ({arrival_x:.0}, {arrival_y:.0})");
        for event in events {
            self.send(event);
        }
        self.state.set_driving(Some(peer_id));
    }

    fn button(&mut self, button: u32, down: bool) {
        let Some(driving) = &mut self.current else { return };
        let bit = 1u32 << (button - 1).min(31);
        if down {
            driving.buttons |= bit;
        } else {
            driving.buttons &= !bit;
        }
        let event = LcInputEvent {
            kind: if down { LC_INPUT_MOUSE_BUTTON_DOWN } else { LC_INPUT_MOUSE_BUTTON_UP },
            code: button,
            buttons: driving.buttons,
            modifiers: driving.modifiers(),
            ..LcInputEvent::default()
        };
        self.send(event);
    }

    fn scroll(&mut self, dx: f64, dy: f64, precise: bool) {
        let Some(driving) = &self.current else { return };
        let factor = driving.input.scroll_speed * if driving.input.invert_scroll { -1.0 } else { 1.0 };
        let event = LcInputEvent {
            kind: LC_INPUT_SCROLL,
            flags: if precise { LC_INPUT_FLAG_PRECISE_SCROLL } else { 0 },
            scroll_x: dx * factor,
            scroll_y: dy * factor,
            buttons: driving.buttons,
            modifiers: driving.modifiers(),
            ..LcInputEvent::default()
        };
        self.send(event);
    }

    fn key(&mut self, usage: u32, down: bool) {
        let Some(driving) = &mut self.current else {
            self.stray_key(usage, down);
            return;
        };
        if down {
            // Ctrl+Alt+Esc brings the pointer home whatever the peer does.
            if usage == USAGE_ESCAPE
                && driving.modifiers() & LC_MODIFIER_CONTROL != 0
                && driving.modifiers() & LC_MODIFIER_OPTION != 0
            {
                let (x, y) = driving.pointer.local_position();
                let home = driving.pointer.return_point(x, y);
                self.end(Some(home));
                return;
            }
        }
        if usage == USAGE_SCROLL_LOCK && self.settings.get().scroll_lock_locks {
            // Kept here rather than sent on: it locks the pointer to the
            // peer, or frees it.
            if down {
                let locked = self.state.set_locked(None);
                log::info!("Scroll Lock: the pointer is {}", if locked { "kept on this device" } else { "free to cross" });
            }
            return;
        }
        let Some(driving) = &mut self.current else { return };
        if down {
            driving.keys.insert(usage);
            driving.repeat = (!(0xE0..=0xE7).contains(&usage))
                .then(|| (usage, Instant::now() + REPEAT_DELAY, 0));
        } else {
            driving.keys.remove(&usage);
            if driving.repeat.is_some_and(|(held, ..)| held == usage) {
                driving.repeat = None;
            }
        }
        let event = LcInputEvent {
            kind: if down { LC_INPUT_KEY_DOWN } else { LC_INPUT_KEY_UP },
            code: usage,
            modifiers: driving.modifiers(),
            buttons: driving.buttons,
            ..LcInputEvent::default()
        };
        self.send(event);
    }

    /// A captured key while nothing is driven: only Ctrl+Alt+Esc means
    /// anything, and it makes the capture let go whatever its state.
    fn stray_key(&mut self, usage: u32, down: bool) {
        if !down {
            self.stray_keys.remove(&usage);
            return;
        }
        let held = |keys: &HashSet<u32>, left: u32, right: u32| keys.contains(&left) || keys.contains(&right);
        if usage == USAGE_ESCAPE
            && held(&self.stray_keys, 0xE0, 0xE4)
            && held(&self.stray_keys, 0xE2, 0xE6)
        {
            log::warn!("Ctrl+Alt+Esc while nothing is driven: telling the capture to let go");
            self.stray_keys.clear();
            if let Some(capture) = &self.capture {
                let _ = capture.send(CaptureCommand::Escape);
            }
            return;
        }
        self.stray_keys.insert(usage);
    }

    /// Hands the pointer back to this desktop at `home`, or just stops
    /// (the compositor already has it). The peer releases whatever it
    /// still holds.
    fn end(&mut self, home: Option<(f64, f64)>) {
        let Some(driving) = self.current.take() else { return };
        if let Some((x, y)) = home
            && let Some(capture) = &self.capture
        {
            let _ = capture.send(CaptureCommand::Release { x, y });
        }
        if let Some(core) = self.core.upgrade() {
            let _ = core.reset_input();
        }
        log::info!("stopped driving {}", driving.peer_id);
        self.state.set_driving(None);
        // Barriers were left alone while driving (a lock, say).
        self.refresh_barriers();
    }

    /// Gives a pointer that just hit a barrier straight back, one point
    /// inward so it does not hit it again at once.
    fn release_at(&self, x: f64, y: f64, side: Option<rinflow_core::layout::Side>) {
        use rinflow_core::layout::Side;
        let (x, y) = match side {
            Some(Side::Right) => (x - 2.0, y),
            Some(Side::Left) => (x + 2.0, y),
            Some(Side::Bottom) => (x, y - 2.0),
            Some(Side::Top) => (x, y + 2.0),
            None => (x, y),
        };
        if let Some(capture) = &self.capture {
            let _ = capture.send(CaptureCommand::Release { x, y });
        }
    }

    fn send(&self, event: LcInputEvent) {
        if let Some(core) = self.core.upgrade() {
            let _ = core.send_input(event);
        }
    }
}

impl Driving {
    fn modifiers(&self) -> u32 {
        let mut bits = 0;
        for &(usage, side, general) in &[
            (0xE0, LC_MODIFIER_LEFT_CONTROL, LC_MODIFIER_CONTROL),
            (0xE4, LC_MODIFIER_RIGHT_CONTROL, LC_MODIFIER_CONTROL),
            (0xE1, LC_MODIFIER_LEFT_SHIFT, LC_MODIFIER_SHIFT),
            (0xE5, LC_MODIFIER_RIGHT_SHIFT, LC_MODIFIER_SHIFT),
            (0xE2, LC_MODIFIER_LEFT_OPTION, LC_MODIFIER_OPTION),
            (0xE6, LC_MODIFIER_RIGHT_OPTION, LC_MODIFIER_OPTION),
            (0xE3, LC_MODIFIER_LEFT_COMMAND, LC_MODIFIER_COMMAND),
            (0xE7, LC_MODIFIER_RIGHT_COMMAND, LC_MODIFIER_COMMAND),
        ] {
            if self.keys.contains(&usage) {
                bits |= side | general;
            }
        }
        bits
    }
}

fn absolute_move(x: f64, y: f64) -> LcInputEvent {
    LcInputEvent {
        kind: LC_INPUT_MOUSE_MOVE,
        flags: LC_INPUT_FLAG_ABSOLUTE_POSITION,
        x,
        y,
        ..LcInputEvent::default()
    }
}

fn relative_move(dx: f64, dy: f64, driving: &Driving) -> LcInputEvent {
    let (x, y) = driving.pointer.position();
    LcInputEvent {
        kind: LC_INPUT_MOUSE_MOVE,
        x,
        y,
        delta_x: dx,
        delta_y: dy,
        buttons: driving.buttons,
        modifiers: driving.modifiers(),
        ..LcInputEvent::default()
    }
}
