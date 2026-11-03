//! Playing a peer's input here, as a libei sender. The connection comes
//! from the RemoteDesktop portal (desktops) or straight from a compositor's
//! EIS socket (gamescope); from the handshake on the code is the same.
//!
//! Motion is relative, as the core sends it; an absolute move only when the
//! core marks one (where the pointer enters). Relative motion is what a
//! game that holds its pointer expects, and what wakes gamescope's hidden
//! cursor.

use crate::ei_common::{handshake, now_micros};
use crate::keymap::{evdev_button, evdev_code};
use crate::token::TokenFile;
use crate::{EisSource, EmulationEvent};
use ashpd::desktop::remote_desktop::{DeviceType, RemoteDesktop, SelectDevicesOptions};
use ashpd::desktop::{PersistMode, Session};
use futures_util::StreamExt;
use rinflow_core::{
    LC_INPUT_FLAG_ABSOLUTE_POSITION, LC_INPUT_FLAG_PRECISE_SCROLL, LC_INPUT_KEY_DOWN,
    LC_INPUT_KEY_UP, LC_INPUT_MOUSE_BUTTON_DOWN, LC_INPUT_MOUSE_BUTTON_UP, LC_INPUT_MOUSE_MOVE,
    LC_INPUT_SCROLL, LcInputEvent,
};
use reis::ei;
use reis::event::{Connection, DeviceCapability, EiEvent};
use std::collections::HashSet;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::os::unix::net::UnixStream;
use std::path::Path;
use tokio::sync::{mpsc, oneshot};

const POINTS_PER_NOTCH: f64 = 10.0;

/// One EIS device that offers an interface, and whether the compositor
/// lets it emulate right now.
struct Slot<T> {
    device: ei::Device,
    interface: T,
    emulating: bool,
    /// The device described at least one region: absolute coordinates
    /// mean something to the compositor. Mutter gives a RemoteDesktop
    /// session's absolute device no region and drops absolute motion.
    has_region: bool,
}

/// Far enough to reach the top-left corner of any desktop from anywhere.
const CORNER: f32 = 1.0e6;

#[derive(Default)]
struct Devices {
    pointer: Option<Slot<ei::Pointer>>,
    absolute: Option<Slot<ei::PointerAbsolute>>,
    button: Option<Slot<ei::Button>>,
    scroll: Option<Slot<ei::Scroll>>,
    keyboard: Option<Slot<ei::Keyboard>>,
    sequence: u32,
    /// The pointer device got its first, one-pixel move: Mutter (51) drops
    /// whatever motion comes in before its pointer has ever moved, the
    /// first absolute entry included; on a desktop in use this is moot.
    nudged: bool,
}

/// Runs until `shutdown` fires, or until the connection is lost or denied
/// (reported as the last event).
pub async fn run(
    source: EisSource,
    state_directory: &Path,
    mut input: mpsc::UnboundedReceiver<LcInputEvent>,
    events: mpsc::UnboundedSender<EmulationEvent>,
    mut shutdown: oneshot::Receiver<()>,
    last_played: Arc<AtomicU64>,
) {
    let report = |event: EmulationEvent| {
        let _ = events.send(event);
    };
    let (fd, session) = match &source {
        EisSource::Portal => match portal_fd(state_directory, &report).await {
            Ok((fd, session)) => (fd, Some(session)),
            Err(event) => {
                report(event);
                return;
            }
        },
        EisSource::Socket(path) => match UnixStream::connect(path) {
            Ok(stream) => (OwnedFd::from(stream), None),
            Err(error) => {
                report(EmulationEvent::Lost(format!("{}: {error}", path.display())));
                return;
            }
        },
    };
    let (context, connection, mut ei_events) =
        match handshake(fd, ei::handshake::ContextType::Sender).await {
            Ok(parts) => parts,
            Err(error) => {
                report(EmulationEvent::Lost(error));
                close(session).await;
                return;
            }
        };
    report(EmulationEvent::Ready);

    let mut devices = Devices::default();
    let mut held_keys: HashSet<u32> = HashSet::new();
    let mut held_buttons: HashSet<u32> = HashSet::new();
    // Fractions of a notch not yet scrolled on a notched wheel.
    let mut notch_rest = (0.0f64, 0.0f64);

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            event = input.recv() => {
                let Some(event) = event else { break };
                last_played.store(unix_millis(), Ordering::Relaxed);
                play(&event, &mut devices, &connection, &mut held_keys, &mut held_buttons, &mut notch_rest);
                if let Err(error) = context.flush() {
                    report(EmulationEvent::Lost(format!("EIS flush: {error}")));
                    break;
                }
            }
            event = ei_events.next() => {
                let event = match event {
                    Some(Ok(event)) => event,
                    Some(Err(error)) => {
                        report(EmulationEvent::Lost(format!("EIS: {error}")));
                        break;
                    }
                    None => {
                        report(EmulationEvent::Lost("EIS connection closed".into()));
                        break;
                    }
                };
                if let Some(lost) = handle_ei_event(event, &mut devices, &connection) {
                    report(EmulationEvent::Lost(lost));
                    break;
                }
                if let Err(error) = context.flush() {
                    report(EmulationEvent::Lost(format!("EIS flush: {error}")));
                    break;
                }
            }
        }
    }
    // Nothing stays pressed on this device after the peer is gone.
    release_all(&mut devices, &connection, &mut held_keys, &mut held_buttons);
    let _ = context.flush();
    close(session).await;
}

async fn close(session: Option<Session<RemoteDesktop>>) {
    if let Some(session) = session {
        let _ = session.close().await;
    }
}

/// The RemoteDesktop portal's EIS connection, asking the user unless a
/// restore token spares them.
async fn portal_fd(
    state_directory: &Path,
    report: &impl Fn(EmulationEvent),
) -> Result<(OwnedFd, Session<RemoteDesktop>), EmulationEvent> {
    let token = TokenFile::new(state_directory, "remote-desktop");
    let portal = RemoteDesktop::new()
        .await
        .map_err(|error| EmulationEvent::Lost(format!("RemoteDesktop portal: {error}")))?;
    let session = portal
        .create_session(Default::default())
        .await
        .map_err(|error| EmulationEvent::Lost(format!("CreateSession: {error}")))?;
    let restore = token.read();
    let options = SelectDevicesOptions::default()
        .set_devices(DeviceType::Keyboard | DeviceType::Pointer)
        .set_persist_mode(PersistMode::ExplicitlyRevoked)
        .set_restore_token(restore.as_deref());
    let selected = portal
        .select_devices(&session, options)
        .await
        .and_then(|request| request.response());
    if let Err(error) = selected {
        let _ = session.close().await;
        return Err(classify(error, "SelectDevices"));
    }
    if restore.is_none() {
        report(EmulationEvent::Prompting);
    }
    let started = portal
        .start(&session, None, Default::default())
        .await
        .and_then(|request| request.response());
    let started = match started {
        Ok(started) => started,
        Err(error) => {
            token.forget();
            let _ = session.close().await;
            return Err(classify(error, "Start"));
        }
    };
    match started.restore_token() {
        Some(new) => token.write(new),
        None => token.forget(),
    }
    match portal.connect_to_eis(&session, Default::default()).await {
        Ok(fd) => Ok((fd, session)),
        Err(error) => {
            let _ = session.close().await;
            Err(EmulationEvent::Lost(format!("ConnectToEIS: {error}")))
        }
    }
}

fn classify(error: ashpd::Error, what: &str) -> EmulationEvent {
    match error {
        ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => {
            EmulationEvent::Denied(format!("{what}: declined"))
        }
        other => EmulationEvent::Lost(format!("{what}: {other}")),
    }
}

/// Tracks seats and devices; `Some(reason)` when the connection is over.
fn handle_ei_event(event: EiEvent, devices: &mut Devices, connection: &Connection) -> Option<String> {
    match event {
        EiEvent::SeatAdded(seat) => {
            seat.seat.bind_capabilities(
                DeviceCapability::Pointer
                    | DeviceCapability::PointerAbsolute
                    | DeviceCapability::Keyboard
                    | DeviceCapability::Button
                    | DeviceCapability::Scroll,
            );
        }
        EiEvent::DeviceAdded(added) => {
            let device = added.device;
            let raw = device.device().clone();
            let has_region = !device.regions().is_empty();
            log::debug!(
                "EIS device {:?} type {:?}: pointer={} abs={} keyboard={} button={} scroll={} regions={:?}",
                device.name(),
                device.device_type(),
                device.has_capability(DeviceCapability::Pointer),
                device.has_capability(DeviceCapability::PointerAbsolute),
                device.has_capability(DeviceCapability::Keyboard),
                device.has_capability(DeviceCapability::Button),
                device.has_capability(DeviceCapability::Scroll),
                device.regions().iter().map(|r| (r.x, r.y, r.width, r.height, r.scale)).collect::<Vec<_>>()
            );
            if let Some(interface) = device.interface::<ei::Pointer>() {
                devices.pointer = Some(Slot { device: raw.clone(), interface, emulating: false, has_region });
            }
            if let Some(interface) = device.interface::<ei::PointerAbsolute>() {
                log::info!("absolute pointer device, regions: {}", device.regions().len());
                devices.absolute = Some(Slot { device: raw.clone(), interface, emulating: false, has_region });
            }
            if let Some(interface) = device.interface::<ei::Button>() {
                devices.button = Some(Slot { device: raw.clone(), interface, emulating: false, has_region });
            }
            if let Some(interface) = device.interface::<ei::Scroll>() {
                devices.scroll = Some(Slot { device: raw.clone(), interface, emulating: false, has_region });
            }
            if let Some(interface) = device.interface::<ei::Keyboard>() {
                devices.keyboard = Some(Slot { device: raw, interface, emulating: false, has_region });
            }
        }
        EiEvent::DeviceResumed(resumed) => {
            // One device carries several interfaces (pointer, button,
            // scroll), so several slots: start_emulating goes out once per
            // device, or the EIS calls it a protocol violation.
            let raw = resumed.device.device();
            if !devices.is_emulating(raw) {
                devices.sequence = devices.sequence.wrapping_add(1);
                raw.start_emulating(connection.serial(), devices.sequence);
            }
            devices.for_device(raw, |_, emulating| *emulating = true);
            if !devices.nudged
                && let Some(slot) = devices.pointer.as_ref().filter(|s| s.emulating && &s.device == raw)
            {
                devices.nudged = true;
                slot.interface.motion_relative(1.0, 0.0);
                frame(slot, connection, now_micros());
                slot.interface.motion_relative(-1.0, 0.0);
                frame(slot, connection, now_micros());
            }
        }
        EiEvent::DevicePaused(paused) => {
            let raw = paused.device.device();
            devices.for_device(raw, |_, emulating| *emulating = false);
        }
        EiEvent::DeviceRemoved(removed) => {
            let raw = removed.device.device();
            devices.remove(raw);
        }
        EiEvent::Disconnected(d) => {
            return Some(format!("EIS disconnected: {:?}", d.reason));
        }
        _ => {}
    }
    None
}

impl Devices {
    fn is_emulating(&self, raw: &ei::Device) -> bool {
        [
            self.pointer.as_ref().map(|s| (&s.device, s.emulating)),
            self.absolute.as_ref().map(|s| (&s.device, s.emulating)),
            self.button.as_ref().map(|s| (&s.device, s.emulating)),
            self.scroll.as_ref().map(|s| (&s.device, s.emulating)),
            self.keyboard.as_ref().map(|s| (&s.device, s.emulating)),
        ]
        .into_iter()
        .flatten()
        .any(|(device, emulating)| device == raw && emulating)
    }

    fn for_device(&mut self, raw: &ei::Device, mut apply: impl FnMut(&ei::Device, &mut bool)) {
        macro_rules! visit {
            ($slot:expr) => {
                if let Some(slot) = $slot.as_mut()
                    && &slot.device == raw
                {
                    apply(&slot.device, &mut slot.emulating);
                }
            };
        }
        visit!(self.pointer);
        visit!(self.absolute);
        visit!(self.button);
        visit!(self.scroll);
        visit!(self.keyboard);
    }

    fn remove(&mut self, raw: &ei::Device) {
        macro_rules! drop_if {
            ($slot:expr) => {
                if $slot.as_ref().is_some_and(|slot| &slot.device == raw) {
                    $slot = None;
                }
            };
        }
        drop_if!(self.pointer);
        drop_if!(self.absolute);
        drop_if!(self.button);
        drop_if!(self.scroll);
        drop_if!(self.keyboard);
    }
}

fn frame<T>(slot: &Slot<T>, connection: &Connection, time: u64) {
    slot.device.frame(connection.serial(), time);
}

fn play(
    event: &LcInputEvent,
    devices: &mut Devices,
    connection: &Connection,
    held_keys: &mut HashSet<u32>,
    held_buttons: &mut HashSet<u32>,
    notch_rest: &mut (f64, f64),
) {
    // The frame is stamped with this machine's monotonic clock: the event's
    // own timestamp is the controller's clock and means nothing here.
    let time = now_micros();
    log::debug!(
        "play kind={} flags={} code={} at=({:.0},{:.0}) delta=({:.1},{:.1}) time={} devices: pointer={:?} abs={:?} kbd={:?}",
        event.kind, event.flags, event.code, event.x, event.y, event.delta_x, event.delta_y, time,
        devices.pointer.as_ref().map(|s| s.emulating),
        devices.absolute.as_ref().map(|s| (s.emulating, s.has_region)),
        devices.keyboard.as_ref().map(|s| s.emulating)
    );
    match event.kind {
        LC_INPUT_MOUSE_MOVE if event.flags & LC_INPUT_FLAG_ABSOLUTE_POSITION != 0 => {
            // Where the pointer enters. A device with a region maps absolute
            // coordinates to the screen (Mutter 46 and 51 both do, exactly).
            // Without one (gamescope) the same point is reached by relative
            // motion from the corner the compositor clamps the pointer into.
            if let Some(slot) = devices.absolute.as_ref().filter(|s| s.emulating && s.has_region) {
                slot.interface.motion_absolute(event.x as f32, event.y as f32);
                frame(slot, connection, time);
            } else if let Some(slot) = devices.pointer.as_ref().filter(|s| s.emulating) {
                slot.interface.motion_relative(-CORNER, -CORNER);
                frame(slot, connection, time);
                slot.interface
                    .motion_relative(event.x as f32, event.y as f32);
                frame(slot, connection, time);
            }
        }
        LC_INPUT_MOUSE_MOVE => {
            if let Some(slot) = devices.pointer.as_ref().filter(|s| s.emulating) {
                slot.interface
                    .motion_relative(event.delta_x as f32, event.delta_y as f32);
                frame(slot, connection, time);
            }
        }
        LC_INPUT_MOUSE_BUTTON_DOWN | LC_INPUT_MOUSE_BUTTON_UP => {
            let down = event.kind == LC_INPUT_MOUSE_BUTTON_DOWN;
            let Some(code) = evdev_button(event.code) else { return };
            if let Some(slot) = devices.button.as_ref().filter(|s| s.emulating) {
                slot.interface.button(code, button_state(down));
                frame(slot, connection, time);
                if down {
                    held_buttons.insert(code);
                } else {
                    held_buttons.remove(&code);
                }
            }
        }
        LC_INPUT_SCROLL => {
            let Some(slot) = devices.scroll.as_ref().filter(|s| s.emulating) else { return };
            // The core: positive toward the top / the left. libei: positive
            // down / right.
            let (dx, dy) = (-event.scroll_x, -event.scroll_y);
            if event.flags & LC_INPUT_FLAG_PRECISE_SCROLL != 0 {
                slot.interface.scroll(dx as f32, dy as f32);
            } else {
                let (nx, rx) = notches(dx, notch_rest.0);
                let (ny, ry) = notches(dy, notch_rest.1);
                *notch_rest = (rx, ry);
                if nx == 0 && ny == 0 {
                    return;
                }
                slot.interface.scroll_discrete(nx * 120, ny * 120);
            }
            frame(slot, connection, time);
        }
        LC_INPUT_KEY_DOWN | LC_INPUT_KEY_UP => {
            let down = event.kind == LC_INPUT_KEY_DOWN;
            let Some(code) = evdev_code(event.code) else { return };
            if let Some(slot) = devices.keyboard.as_ref().filter(|s| s.emulating) {
                slot.interface.key(code, key_state(down));
                frame(slot, connection, time);
                if down {
                    held_keys.insert(code);
                } else {
                    held_keys.remove(&code);
                }
            }
        }
        _ => {}
    }
}

/// Whole notches in `points` plus what was left over, and the new remainder.
fn notches(points: f64, rest: f64) -> (i32, f64) {
    let total = points / POINTS_PER_NOTCH + rest;
    let whole = total.trunc();
    (whole as i32, total - whole)
}

fn release_all(
    devices: &mut Devices,
    connection: &Connection,
    held_keys: &mut HashSet<u32>,
    held_buttons: &mut HashSet<u32>,
) {
    let time = now_micros();
    if let Some(slot) = devices.keyboard.as_ref().filter(|s| s.emulating) {
        for code in held_keys.drain() {
            slot.interface.key(code, key_state(false));
            frame(slot, connection, time);
        }
    }
    if let Some(slot) = devices.button.as_ref().filter(|s| s.emulating) {
        for code in held_buttons.drain() {
            slot.interface.button(code, button_state(false));
            frame(slot, connection, time);
        }
    }
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn button_state(down: bool) -> ei::button::ButtonState {
    if down {
        ei::button::ButtonState::Press
    } else {
        ei::button::ButtonState::Released
    }
}

fn key_state(down: bool) -> ei::keyboard::KeyState {
    if down {
        ei::keyboard::KeyState::Press
    } else {
        ei::keyboard::KeyState::Released
    }
}

#[cfg(test)]
mod tests {
    use super::notches;

    #[test]
    fn notched_scrolling_keeps_fractions() {
        assert_eq!(notches(10.0, 0.0), (1, 0.0));
        assert_eq!(notches(-30.0, 0.0), (-3, 0.0));
        let (whole, rest) = notches(5.0, 0.0);
        assert_eq!(whole, 0);
        assert_eq!(notches(5.0, rest).0, 1);
    }
}
