//! Capture on an X11 session: what the InputCapture portal does on
//! Wayland, done with the X server's own means.
//!
//! - **Edges**: XFixes pointer barriers on the lines the agent names. The
//!   server stops the pointer there and says so (XInput 2.3 `BarrierHit`,
//!   with the point it stopped at), so the hit point is exact and nothing
//!   has to watch the pointer move.
//! - **The hit**: the pointer is grabbed and kept where it stopped; its
//!   motion comes as XInput raw events, which carry what the mouse did
//!   whatever the pointer on screen can do. That is all until the agent
//!   says the crossing is on ([`CaptureCommand::Engage`]): an edge that
//!   leads nowhere right now (a device that is off, a corner, the edge
//!   delay) must not take the keyboard from the application in use, which
//!   on X11 closes its menus.
//! - **The crossing**: the keyboard is grabbed too and the pointer hidden;
//!   keys and buttons come as raw events as well.
//! - **Giving back**: the grabs end and the pointer is put where the agent
//!   says.
//!
//! The grabs are on a window of this backend's own, never the root: a
//! client holding a grab on the root window gets no raw events for it.
//! Raw events are selected only while a hit lasts, so the agent sees no
//! key or motion of a desktop it is not capturing.
//!
//! While another program holds the pointer (an open menu, a drag) the
//! grab is refused and the hit is ignored, as Deskflow does: the next
//! push against the edge tries again.
//!
//! Deskflow's X11 code (src/lib/platform/XWindowsComputer.cpp) was read
//! for this. It watches the pointer's position and warps it to the middle
//! of the screen to measure motion; barriers and raw events replace both.

use crate::keymap::hid_usage;
use crate::x11_common::{Link, X11Display, fp1616, fp3232, pixel};
use crate::{Barrier, CaptureCommand, CaptureEvent};
use rinflow_core::layout::{Display, Side};
use std::collections::{HashMap, HashSet};
use std::fmt::Display as Shown;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::{self, BarrierDirections};
use x11rb::protocol::xinput::{self, XIEventMask};
use x11rb::protocol::xproto::{self, ConnectionExt as _, GrabMode, GrabStatus, Window};

/// Points per notch of a wheel: the core's convention.
const POINTS_PER_NOTCH: f64 = 10.0;
/// XInput's name for "every master device".
const ALL_MASTER_DEVICES: u16 = 1;
/// After a hit that led nowhere, the edge is left alone for this long:
/// the user is still pushing, and every hit grabs the pointer.
const QUIET_AFTER_REFUSAL: Duration = Duration::from_millis(250);

/// Runs until `Shutdown`, or until the X server is gone (reported as the
/// last event).
pub async fn run(
    display: X11Display,
    mut commands: mpsc::UnboundedReceiver<CaptureCommand>,
    events: mpsc::UnboundedSender<CaptureEvent>,
) {
    let report = |event: CaptureEvent| {
        let _ = events.send(event);
    };
    let mut capture = match Capture::open(&display) {
        Ok(capture) => capture,
        Err(error) => {
            report(CaptureEvent::Lost(error));
            return;
        }
    };
    report(CaptureEvent::Zones(capture.displays.clone()));
    report(CaptureEvent::Ready);

    let result: Result<(), String> = async {
        loop {
            // Everything queued first: x11rb also queues what arrives
            // while it waits for a reply, and the socket is silent then.
            while let Some(event) = capture.link.conn.poll_for_event().map_err(text)? {
                capture.handle(event, &report)?;
            }
            capture.settle()?;
            tokio::select! {
                command = commands.recv() => match command {
                    Some(CaptureCommand::SetBarriers(barriers)) => capture.set_barriers(barriers)?,
                    Some(CaptureCommand::Engage) => {
                        if !capture.engage()? {
                            report(CaptureEvent::Deactivated);
                        }
                    }
                    Some(CaptureCommand::Release { x, y }) => {
                        if capture.release(Some((x, y)))? {
                            report(CaptureEvent::Deactivated);
                        }
                    }
                    Some(CaptureCommand::Escape) => {
                        log::info!("escape: letting go of the pointer and the keyboard");
                        capture.release(None)?;
                        // Whatever was held is over.
                        report(CaptureEvent::Deactivated);
                    }
                    Some(CaptureCommand::Shutdown) | None => return Ok(()),
                },
                readable = capture.link.readable() => readable.map_err(text)?,
            }
        }
    }
    .await;
    // The server lets go of everything when the connection closes; this
    // puts the pointer back in sight first while it still can.
    let _ = capture.release(None);
    if let Err(error) = result {
        report(CaptureEvent::Lost(format!("X server: {error}")));
    }
}

fn text(error: impl Shown) -> String {
    error.to_string()
}

struct Capture {
    link: Link,
    /// The window the grabs are on: input only, never seen.
    window: Window,
    scale: f64,
    displays: Vec<Display>,
    /// The agent's barriers and the server's ID for each.
    barriers: Vec<(Barrier, xfixes::Barrier)>,
    held: Option<Held>,
    /// Hits are ignored until then (see `QUIET_AFTER_REFUSAL`).
    quiet_until: Option<Instant>,
    /// When a refused grab was last logged.
    refusal_logged: Option<Instant>,
}

/// A pointer stopped at a barrier and kept there.
struct Held {
    /// Where it stopped, in pixels.
    x: i16,
    y: i16,
    /// The crossing is on: the keyboard is grabbed, the pointer hidden.
    engaged: bool,
    /// Keys reported down, so a repeat is not reported again.
    keys: HashSet<u32>,
    /// Whether a device reports motion as deltas (a mouse, a touchpad) or
    /// as positions (a tablet, a touch screen, a virtual machine's
    /// pointer), by its ID.
    relative: HashMap<u16, bool>,
    /// The pointer moved off its spot since it was last put back.
    moved: bool,
}

impl Capture {
    fn open(display: &X11Display) -> Result<Self, String> {
        let link = Link::open(display)?;
        let conn = &link.conn;
        let fixes = xfixes::query_version(conn, 5, 0).map_err(text)?.reply().map_err(|_| "the XFixes extension is missing".to_owned())?;
        if fixes.major_version < 5 {
            return Err(format!("XFixes {}.{} has no pointer barriers (5.0 is needed)", fixes.major_version, fixes.minor_version));
        }
        let input = xinput::xi_query_version(conn, 2, 3).map_err(text)?.reply().map_err(|_| "the XInput extension is missing".to_owned())?;
        if (input.major_version, input.minor_version) < (2, 3) {
            return Err(format!("XInput {}.{} does not report barrier hits (2.3 is needed)", input.major_version, input.minor_version));
        }
        link.watch_geometry();
        let window = conn.generate_id().map_err(text)?;
        conn.create_window(
            0,
            window,
            link.root,
            -10,
            -10,
            1,
            1,
            0,
            xproto::WindowClass::INPUT_ONLY,
            x11rb::COPY_FROM_PARENT,
            &xproto::CreateWindowAux::new().override_redirect(1),
        )
        .map_err(text)?;
        // A grab needs its window mapped.
        conn.map_window(window).map_err(text)?;
        let scale = link.scale();
        let displays = link.displays(scale);
        let capture = Self { link, window, scale, displays, barriers: Vec::new(), held: None, quiet_until: None, refusal_logged: None };
        capture.select(XIEventMask::BARRIER_HIT)?;
        log::info!("X11 capture: XFixes {}.{}, XInput {}.{}, scale {}", fixes.major_version, fixes.minor_version, input.major_version, input.minor_version, scale);
        Ok(capture)
    }

    /// The XInput events wanted from the root window, for every device.
    fn select(&self, mask: XIEventMask) -> Result<(), String> {
        let masks = [xinput::EventMask { deviceid: ALL_MASTER_DEVICES, mask: vec![mask] }];
        xinput::xi_select_events(&self.link.conn, self.link.root, &masks).map_err(text)?;
        Ok(())
    }

    fn set_barriers(&mut self, barriers: Vec<Barrier>) -> Result<(), String> {
        let conn = &self.link.conn;
        for (_, id) in self.barriers.drain(..) {
            xfixes::delete_pointer_barrier(conn, id).map_err(text)?;
        }
        for barrier in barriers {
            let id = conn.generate_id().map_err(text)?;
            let (x1, y1, x2, y2, directions) = barrier_line(&barrier, self.scale);
            xfixes::create_pointer_barrier(conn, id, self.link.root, x1, y1, x2, y2, directions, &[]).map_err(text)?;
            self.barriers.push((barrier, id));
        }
        log::info!(
            "barriers to the X server: {:?}",
            self.barriers.iter().map(|(barrier, _)| (barrier.id, barrier_line(barrier, self.scale))).collect::<Vec<_>>()
        );
        Ok(())
    }

    fn handle(&mut self, event: Event, report: &impl Fn(CaptureEvent)) -> Result<(), String> {
        if self.link.concerns_geometry(&event) {
            let scale = self.link.scale();
            let displays = self.link.displays(scale);
            if displays != self.displays {
                log::info!("X11 capture: the displays changed (scale {scale})");
                self.scale = scale;
                self.displays = displays.clone();
                // The agent answers with barriers for the new edges.
                report(CaptureEvent::Zones(displays));
            }
            return Ok(());
        }
        match event {
            Event::XinputBarrierHit(hit) => self.hit(&hit, report)?,
            Event::XinputRawMotion(motion) => {
                let scale = self.scale;
                let Some(held) = &mut self.held else { return Ok(()) };
                let relative = *held.relative.entry(motion.sourceid).or_insert_with(|| is_relative(&self.link, motion.sourceid));
                if !relative {
                    // Positions, not motion: a pointer that is where the
                    // pen or finger is cannot go past this screen.
                    return Ok(());
                }
                let (dx, dy) = deltas(&motion.valuator_mask, &motion.axisvalues);
                if dx != 0.0 || dy != 0.0 {
                    held.moved = true;
                    report(CaptureEvent::Motion { dx: dx / scale, dy: dy / scale });
                }
            }
            Event::XinputRawButtonPress(button) if self.held.is_some() => match button.detail {
                // The wheel, as X has always had it: a click per notch.
                4 => report(CaptureEvent::Scroll { dx: 0.0, dy: POINTS_PER_NOTCH, precise: false }),
                5 => report(CaptureEvent::Scroll { dx: 0.0, dy: -POINTS_PER_NOTCH, precise: false }),
                6 => report(CaptureEvent::Scroll { dx: POINTS_PER_NOTCH, dy: 0.0, precise: false }),
                7 => report(CaptureEvent::Scroll { dx: -POINTS_PER_NOTCH, dy: 0.0, precise: false }),
                detail => {
                    if let Some(button) = button_number(detail) {
                        report(CaptureEvent::Button { button, down: true });
                    }
                }
            },
            Event::XinputRawButtonRelease(button) if self.held.is_some() => {
                if let Some(button) = button_number(button.detail) {
                    report(CaptureEvent::Button { button, down: false });
                }
            }
            Event::XinputRawKeyPress(key) => {
                let Some(held) = &mut self.held else { return Ok(()) };
                // The server repeats a held key; the agent repeats by itself.
                if let Some(usage) = key_usage(key.detail)
                    && held.keys.insert(usage)
                {
                    report(CaptureEvent::Key { usage, down: true });
                }
            }
            Event::XinputRawKeyRelease(key) => {
                let Some(held) = &mut self.held else { return Ok(()) };
                if let Some(usage) = key_usage(key.detail) {
                    held.keys.remove(&usage);
                    report(CaptureEvent::Key { usage, down: false });
                }
            }
            Event::Error(error) => log::debug!("X11 capture: the server refused a request: {error:?}"),
            _ => {}
        }
        Ok(())
    }

    /// The pointer stopped at one of the barriers.
    fn hit(&mut self, hit: &xinput::BarrierHitEvent, report: &impl Fn(CaptureEvent)) -> Result<(), String> {
        if self.held.is_some() {
            return Ok(());
        }
        let now = Instant::now();
        if self.quiet_until.is_some_and(|until| now < until) {
            return Ok(());
        }
        let Some(barrier) = self.barriers.iter().find(|(_, id)| *id == hit.barrier).map(|(barrier, _)| barrier.id) else {
            return Ok(());
        };
        let conn = &self.link.conn;
        let status = conn
            .grab_pointer(false, self.window, xproto::EventMask::NO_EVENT, GrabMode::ASYNC, GrabMode::ASYNC, x11rb::NONE, x11rb::NONE, x11rb::CURRENT_TIME)
            .map_err(text)?
            .reply()
            .map_err(text)?
            .status;
        if status != GrabStatus::SUCCESS {
            // A menu is open, or something is being dragged.
            if self.refusal_logged.is_none_or(|when| now.duration_since(when) > Duration::from_secs(5)) {
                self.refusal_logged = Some(now);
                log::info!("the pointer is at an edge, but another program holds it ({status:?}): not crossing");
            }
            return Ok(());
        }
        self.select(XIEventMask::BARRIER_HIT | XIEventMask::RAW_MOTION | XIEventMask::RAW_BUTTON_PRESS | XIEventMask::RAW_BUTTON_RELEASE)?;
        let (x, y) = (fp1616(hit.root_x), fp1616(hit.root_y));
        self.held = Some(Held { x: x as i16, y: y as i16, engaged: false, keys: HashSet::new(), relative: HashMap::new(), moved: false });
        log::debug!("capture activated: barrier {barrier} at ({x:.0}, {y:.0}) pixels");
        report(CaptureEvent::Activated { barrier, x: x / self.scale, y: y / self.scale });
        Ok(())
    }

    /// The crossing is on: the keyboard goes with the pointer. False when
    /// another program holds the keyboard (the pointer is given back
    /// then), or when no pointer is held to go with: either way nothing is
    /// captured, and the agent must not think otherwise.
    fn engage(&mut self) -> Result<bool, String> {
        let Some(held) = &mut self.held else { return Ok(false) };
        if held.engaged {
            return Ok(true);
        }
        let conn = &self.link.conn;
        let status = conn
            .grab_keyboard(false, self.window, x11rb::CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)
            .map_err(text)?
            .reply()
            .map_err(text)?
            .status;
        if status != GrabStatus::SUCCESS {
            log::warn!("another program holds the keyboard ({status:?}): not crossing");
            self.release(None)?;
            return Ok(false);
        }
        xfixes::hide_cursor(conn, self.link.root).map_err(text)?;
        held.engaged = true;
        self.select(
            XIEventMask::BARRIER_HIT
                | XIEventMask::RAW_MOTION
                | XIEventMask::RAW_BUTTON_PRESS
                | XIEventMask::RAW_BUTTON_RELEASE
                | XIEventMask::RAW_KEY_PRESS
                | XIEventMask::RAW_KEY_RELEASE,
        )?;
        Ok(true)
    }

    /// Gives the pointer (and the keyboard) back, the pointer at
    /// `position` in points when given. False when nothing was held.
    fn release(&mut self, position: Option<(f64, f64)>) -> Result<bool, String> {
        let conn = &self.link.conn;
        let Some(held) = self.held.take() else {
            // Nothing known to be held; these are harmless then, and make
            // sure of it.
            conn.ungrab_keyboard(x11rb::CURRENT_TIME).map_err(text)?;
            conn.ungrab_pointer(x11rb::CURRENT_TIME).map_err(text)?;
            conn.flush().map_err(text)?;
            return Ok(false);
        };
        self.select(XIEventMask::BARRIER_HIT)?;
        let conn = &self.link.conn;
        if held.engaged {
            conn.ungrab_keyboard(x11rb::CURRENT_TIME).map_err(text)?;
            xfixes::show_cursor(conn, self.link.root).map_err(text)?;
        } else {
            self.quiet_until = Some(Instant::now() + QUIET_AFTER_REFUSAL);
        }
        let (x, y) = match position {
            Some((x, y)) => (pixel(x, self.scale), pixel(y, self.scale)),
            None => (held.x, held.y),
        };
        conn.warp_pointer(x11rb::NONE, self.link.root, 0, 0, 0, 0, x, y).map_err(text)?;
        conn.ungrab_pointer(x11rb::CURRENT_TIME).map_err(text)?;
        conn.flush().map_err(text)?;
        log::debug!("capture released at ({x}, {y}) pixels");
        Ok(true)
    }

    /// After a batch of events: a held pointer goes back to its spot (the
    /// server moved it with the mouse; a warp is not motion, so this does
    /// not come back as an event), and what was asked of the server goes
    /// out.
    fn settle(&mut self) -> Result<(), String> {
        if let Some(held) = &mut self.held
            && std::mem::take(&mut held.moved)
        {
            self.link.conn.warp_pointer(x11rb::NONE, self.link.root, 0, 0, 0, 0, held.x, held.y).map_err(text)?;
        }
        self.link.conn.flush().map_err(text)
    }
}

/// A barrier as XFixes wants it: a line between pixels, and the
/// directions the pointer may still pass in (back into the screen).
fn barrier_line(barrier: &Barrier, scale: f64) -> (u16, u16, u16, u16, BarrierDirections) {
    let pixel = |points: f64| (points * scale).round().clamp(0.0, f64::from(u16::MAX)) as u16;
    let (line, from, to) = (pixel(barrier.line), pixel(barrier.from), pixel(barrier.to));
    match barrier.side {
        Side::Right => (line, from, line, to, BarrierDirections::NEGATIVE_X),
        Side::Left => (line, from, line, to, BarrierDirections::POSITIVE_X),
        Side::Bottom => (from, line, to, line, BarrierDirections::NEGATIVE_Y),
        Side::Top => (from, line, to, line, BarrierDirections::POSITIVE_Y),
    }
}

/// Motion on the first two axes of a raw event: the mask says which axes
/// the event carries, the values follow in that order.
fn deltas(mask: &[u32], values: &[xinput::Fp3232]) -> (f64, f64) {
    let mut delta = (0.0, 0.0);
    let mut values = values.iter();
    for (word, bits) in mask.iter().enumerate() {
        for bit in 0..32 {
            if bits & (1 << bit) == 0 {
                continue;
            }
            let Some(value) = values.next() else { return delta };
            match word * 32 + bit {
                0 => delta.0 = fp3232(*value),
                1 => delta.1 = fp3232(*value),
                _ => return delta,
            }
        }
    }
    delta
}

/// Whether a device's first axis counts motion (true when it cannot be
/// asked).
fn is_relative(link: &Link, device: u16) -> bool {
    let Some(reply) = xinput::xi_query_device(&link.conn, device).ok().and_then(|cookie| cookie.reply().ok()) else {
        return true;
    };
    reply
        .infos
        .iter()
        .flat_map(|info| &info.classes)
        .find_map(|class| match &class.data {
            xinput::DeviceClassData::Valuator(axis) if axis.number == 0 => Some(axis.mode == xinput::ValuatorMode::RELATIVE),
            _ => None,
        })
        .unwrap_or(true)
}

/// The HID usage of an X key code: the evdev code plus eight, on every
/// server that takes its keyboards from the kernel.
fn key_usage(keycode: u32) -> Option<u32> {
    hid_usage(keycode.checked_sub(8)?)
}

/// X's button numbers (1 left, 2 middle, 3 right, 8 back, 9 forward and
/// on; 4 to 7 are the wheel) → the core's (1 left, 2 right, 3 middle, 4
/// back, 5 forward and on).
pub(crate) fn button_number(x_button: u32) -> Option<u32> {
    Some(match x_button {
        1 => 1,
        2 => 3,
        3 => 2,
        8..=12 => x_button - 4,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xinput::Fp3232;

    #[test]
    fn barriers_block_the_way_out_of_the_screen() {
        let right = Barrier { id: 1, side: Side::Right, line: 1920.0, from: 0.0, to: 1080.0 };
        assert_eq!(barrier_line(&right, 1.0), (1920, 0, 1920, 1080, BarrierDirections::NEGATIVE_X));
        let top = Barrier { id: 2, side: Side::Top, line: 0.0, from: 100.0, to: 500.0 };
        assert_eq!(barrier_line(&top, 1.0), (100, 0, 500, 0, BarrierDirections::POSITIVE_Y));
        // Points to pixels on a desktop at 200 %.
        let left = Barrier { id: 3, side: Side::Left, line: 0.0, from: 90.0, to: 990.0 };
        assert_eq!(barrier_line(&left, 2.0), (0, 180, 0, 1980, BarrierDirections::POSITIVE_X));
        let bottom = Barrier { id: 4, side: Side::Bottom, line: 1080.0, from: 0.0, to: 1920.0 };
        assert_eq!(barrier_line(&bottom, 2.0), (0, 2160, 3840, 2160, BarrierDirections::NEGATIVE_Y));
    }

    #[test]
    fn raw_motion_is_read_by_its_axis_mask() {
        let fp = |integral: i32| Fp3232 { integral, frac: 0 };
        assert_eq!(deltas(&[0b11], &[fp(7), fp(-2)]), (7.0, -2.0));
        // Only the second axis moved.
        assert_eq!(deltas(&[0b10], &[fp(5)]), (0.0, 5.0));
        // A wheel's axes come after the first two and are not motion.
        assert_eq!(deltas(&[0b1101], &[fp(3), fp(15), fp(9)]), (3.0, 0.0));
        assert_eq!(deltas(&[0b1000], &[fp(15)]), (0.0, 0.0));
        assert_eq!(deltas(&[], &[]), (0.0, 0.0));
        assert_eq!(deltas(&[0b1], &[Fp3232 { integral: 1, frac: 1 << 31 }]), (1.5, 0.0));
    }

    #[test]
    fn keys_and_buttons_map_from_what_x_calls_them() {
        assert_eq!(key_usage(38), Some(0x04)); // a
        assert_eq!(key_usage(9), Some(0x29)); // Escape
        assert_eq!(key_usage(3), None);
        assert_eq!(button_number(1), Some(1));
        assert_eq!(button_number(3), Some(2));
        assert_eq!(button_number(2), Some(3));
        assert_eq!(button_number(8), Some(4));
        assert_eq!(button_number(9), Some(5));
        assert_eq!(button_number(5), None);
    }
}
