//! Playing a peer's input on an X11 session, through the XTest extension:
//! the server treats what it is given as if a device had sent it.
//!
//! Motion is relative, as the core sends it; an absolute move only when the
//! core marks one (where the pointer enters). Keys go by position (the X
//! key code is the evdev code plus eight), so the layout of this desktop
//! decides what they type, as with a keyboard plugged into it.
//!
//! What X cannot do: scroll by less than a wheel notch. XTest has no
//! scroll axis, only the wheel's buttons, so smooth scrolling from a
//! touchpad is gathered into notches.
//!
//! From Deskflow's X11 client (src/lib/platform/XWindowsComputer.cpp):
//! XTest is told to go on through another program's server grab, which
//! would otherwise stop the peer's input dead for as long as it lasts; and
//! a screen that power management turned off is turned on when the pointer
//! enters, since played input does not always wake it.

use crate::ei_emulation::notches;
use crate::keymap::evdev_code;
use crate::x11_clipboard;
use crate::x11_common::{Link, X11Display, pixel};
use crate::{ClipboardLink, EmulationEvent};
use rinflow_core::{
    LC_INPUT_FLAG_ABSOLUTE_POSITION, LC_INPUT_KEY_DOWN, LC_INPUT_KEY_UP, LC_INPUT_MOUSE_BUTTON_DOWN,
    LC_INPUT_MOUSE_BUTTON_UP, LC_INPUT_MOUSE_MOVE, LC_INPUT_SCROLL, LcInputEvent,
};
use std::collections::HashSet;
use std::fmt::Display as Shown;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, oneshot};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::protocol::{Event, dpms, xtest};

/// The most wheel clicks one event may turn into: a runaway value must
/// not flood the server.
const MOST_NOTCHES: i32 = 40;

/// Runs until `shutdown` fires, or until the X server is gone (reported as
/// the last event). With `clipboard`, the session's clipboard is served
/// for as long as this runs.
pub async fn run(
    display: X11Display,
    mut input: mpsc::UnboundedReceiver<LcInputEvent>,
    events: mpsc::UnboundedSender<EmulationEvent>,
    mut shutdown: oneshot::Receiver<()>,
    last_played: Arc<AtomicU64>,
    clipboard: Option<ClipboardLink>,
) {
    let report = |event: EmulationEvent| {
        let _ = events.send(event);
    };
    let mut player = match Player::open(&display) {
        Ok(player) => player,
        Err(error) => {
            report(EmulationEvent::Lost(error));
            return;
        }
    };
    report(EmulationEvent::Ready);
    // The clipboard has its own connection and task: an application slow
    // to hand over a large image must not hold up the peer's pointer.
    let clipboard_task = clipboard.map(|link| tokio::task::spawn_local(x11_clipboard::serve(display.clone(), link.commands, link.events)));

    let result: Result<(), String> = async {
        loop {
            while let Some(event) = player.link.conn.poll_for_event().map_err(text)? {
                match event {
                    event if player.link.concerns_geometry(&event) => player.scale = player.link.scale(),
                    Event::Error(error) => log::debug!("X11 emulation: the server refused a request: {error:?}"),
                    _ => {}
                }
            }
            player.link.conn.flush().map_err(text)?;
            tokio::select! {
                _ = &mut shutdown => return Ok(()),
                event = input.recv() => {
                    let Some(event) = event else { return Ok(()) };
                    last_played.store(unix_millis(), Ordering::Relaxed);
                    player.play(&event)?;
                }
                readable = player.link.readable() => readable.map_err(text)?,
            }
        }
    }
    .await;
    // Nothing stays pressed on this device after the peer is gone.
    let _ = player.release_all();
    if let Some(task) = clipboard_task {
        task.abort();
    }
    if let Err(error) = result {
        report(EmulationEvent::Lost(format!("X server: {error}")));
    }
}

fn text(error: impl Shown) -> String {
    error.to_string()
}

struct Player {
    link: Link,
    scale: f64,
    /// The key codes this server has.
    keycodes: std::ops::RangeInclusive<u8>,
    /// Power management can be asked about and told.
    dpms: bool,
    held_keys: HashSet<u8>,
    held_buttons: HashSet<u8>,
    /// Fractions of a pixel not yet moved.
    pixel_rest: (f64, f64),
    /// Fractions of a notch not yet scrolled.
    notch_rest: (f64, f64),
}

impl Player {
    fn open(display: &X11Display) -> Result<Self, String> {
        let link = Link::open(display)?;
        let conn = &link.conn;
        xtest::get_version(conn, 2, 2).map_err(text)?.reply().map_err(|_| "the XTest extension is missing".to_owned())?;
        xtest::grab_control(conn, true).map_err(text)?;
        let dpms = dpms::get_version(conn, 1, 1).ok().and_then(|cookie| cookie.reply().ok()).is_some();
        link.watch_geometry();
        let setup = conn.setup();
        let keycodes = setup.min_keycode..=setup.max_keycode;
        let scale = link.scale();
        log::info!("X11 emulation: XTest, scale {scale}");
        Ok(Self { link, scale, keycodes, dpms, held_keys: HashSet::new(), held_buttons: HashSet::new(), pixel_rest: (0.0, 0.0), notch_rest: (0.0, 0.0) })
    }

    fn play(&mut self, event: &LcInputEvent) -> Result<(), String> {
        log::debug!(
            "play kind={} flags={} code={} at=({:.0},{:.0}) delta=({:.1},{:.1})",
            event.kind, event.flags, event.code, event.x, event.y, event.delta_x, event.delta_y
        );
        match event.kind {
            LC_INPUT_MOUSE_MOVE if event.flags & LC_INPUT_FLAG_ABSOLUTE_POSITION != 0 => {
                // Where the pointer enters: the peer's user has arrived.
                self.wake()?;
                self.pixel_rest = (0.0, 0.0);
                self.fake(xproto::MOTION_NOTIFY_EVENT, 0, self.link.root, pixel(event.x, self.scale), pixel(event.y, self.scale))?;
            }
            LC_INPUT_MOUSE_MOVE => {
                let (dx, rest_x) = whole(event.delta_x * self.scale + self.pixel_rest.0);
                let (dy, rest_y) = whole(event.delta_y * self.scale + self.pixel_rest.1);
                self.pixel_rest = (rest_x, rest_y);
                if dx != 0 || dy != 0 {
                    // Detail 1: relative to where the pointer is.
                    self.fake(xproto::MOTION_NOTIFY_EVENT, 1, x11rb::NONE, dx, dy)?;
                }
            }
            LC_INPUT_MOUSE_BUTTON_DOWN | LC_INPUT_MOUSE_BUTTON_UP => {
                let down = event.kind == LC_INPUT_MOUSE_BUTTON_DOWN;
                let Some(button) = x_button(event.code) else { return Ok(()) };
                self.fake(if down { xproto::BUTTON_PRESS_EVENT } else { xproto::BUTTON_RELEASE_EVENT }, button, x11rb::NONE, 0, 0)?;
                if down {
                    self.held_buttons.insert(button);
                } else {
                    self.held_buttons.remove(&button);
                }
            }
            LC_INPUT_SCROLL => {
                // The core: positive toward the top / the left, which are
                // the wheel's buttons 4 and 6; 5 and 7 the other way.
                let (across, rest_x) = notches(event.scroll_x, self.notch_rest.0);
                let (along, rest_y) = notches(event.scroll_y, self.notch_rest.1);
                self.notch_rest = (rest_x, rest_y);
                self.click(if along > 0 { 4 } else { 5 }, along)?;
                self.click(if across > 0 { 6 } else { 7 }, across)?;
            }
            LC_INPUT_KEY_DOWN | LC_INPUT_KEY_UP => {
                let down = event.kind == LC_INPUT_KEY_DOWN;
                let Some(keycode) = evdev_code(event.code).and_then(|code| u8::try_from(code + 8).ok()).filter(|keycode| self.keycodes.contains(keycode))
                else {
                    return Ok(());
                };
                self.fake(if down { xproto::KEY_PRESS_EVENT } else { xproto::KEY_RELEASE_EVENT }, keycode, x11rb::NONE, 0, 0)?;
                if down {
                    self.held_keys.insert(keycode);
                } else {
                    self.held_keys.remove(&keycode);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn fake(&self, kind: u8, detail: u8, root: xproto::Window, x: i16, y: i16) -> Result<(), String> {
        xtest::fake_input(&self.link.conn, kind, detail, x11rb::CURRENT_TIME, root, x, y, 0).map_err(text)?;
        Ok(())
    }

    /// A wheel button pressed and released, once per notch.
    fn click(&self, button: u8, notches: i32) -> Result<(), String> {
        for _ in 0..notches.abs().min(MOST_NOTCHES) {
            self.fake(xproto::BUTTON_PRESS_EVENT, button, x11rb::NONE, 0, 0)?;
            self.fake(xproto::BUTTON_RELEASE_EVENT, button, x11rb::NONE, 0, 0)?;
        }
        Ok(())
    }

    /// The screen comes on and the screen saver's clock starts over.
    fn wake(&self) -> Result<(), String> {
        let conn = &self.link.conn;
        if self.dpms
            && let Some(info) = dpms::info(conn).ok().and_then(|cookie| cookie.reply().ok())
            && info.state
            && info.power_level != dpms::DPMSMode::ON
        {
            log::info!("X11 emulation: turning the screen on for the peer");
            dpms::force_level(conn, dpms::DPMSMode::ON).map_err(text)?;
        }
        conn.force_screen_saver(xproto::ScreenSaver::RESET).map_err(text)?;
        Ok(())
    }

    fn release_all(&mut self) -> Result<(), String> {
        for keycode in std::mem::take(&mut self.held_keys) {
            self.fake(xproto::KEY_RELEASE_EVENT, keycode, x11rb::NONE, 0, 0)?;
        }
        for button in std::mem::take(&mut self.held_buttons) {
            self.fake(xproto::BUTTON_RELEASE_EVENT, button, x11rb::NONE, 0, 0)?;
        }
        self.link.conn.flush().map_err(text)
    }
}

/// The whole pixels in a distance and what is left of it, within what one
/// request can carry.
fn whole(pixels: f64) -> (i16, f64) {
    let whole = pixels.trunc().clamp(f64::from(i16::MIN), f64::from(i16::MAX));
    (whole as i16, pixels - whole)
}

/// The core's button numbers (1 left, 2 right, 3 middle, 4 back, 5 forward
/// and on) → X's (1 left, 2 middle, 3 right, 8 back, 9 forward and on; 4
/// to 7 are the wheel).
fn x_button(button: u32) -> Option<u8> {
    Some(match button {
        1 => 1,
        2 => 3,
        3 => 2,
        4..=8 => button as u8 + 4,
        _ => return None,
    })
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x11_capture::button_number;

    #[test]
    fn buttons_come_back_as_they_went() {
        for button in 1..=8 {
            assert_eq!(button_number(u32::from(x_button(button).unwrap())), Some(button));
        }
        assert_eq!(x_button(1), Some(1));
        assert_eq!(x_button(2), Some(3));
        assert_eq!(x_button(4), Some(8));
        assert_eq!(x_button(0), None);
        assert_eq!(x_button(9), None);
    }

    #[test]
    fn fractions_of_a_pixel_are_kept_for_the_next_move() {
        assert_eq!(whole(2.75), (2, 0.75));
        assert_eq!(whole(-2.75), (-2, -0.75));
        let (first, rest) = whole(0.6);
        assert_eq!(first, 0);
        assert_eq!(whole(0.6 + rest).0, 1);
        assert_eq!(whole(1.0e9).0, i16::MAX);
    }
}
