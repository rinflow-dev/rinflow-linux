//! Input on a Linux desktop, in both directions, behind two small
//! interfaces the agent drives over channels:
//!
//! - **Capture** (this device drives a peer): invisible barriers on the
//!   edges the agent names; when the pointer hits one, applications stop
//!   seeing input and it comes here until we release it at a point of our
//!   choosing. On Wayland the compositor does that and hands the input
//!   over libei: the `org.freedesktop.portal.InputCapture` portal (GNOME
//!   ≥ 45, Plasma ≥ 6.1). On X11 the X server does: XFixes barriers, grabs
//!   and XInput raw events (`x11_capture.rs`).
//! - **Emulation** (a peer drives this device): on Wayland a libei sender
//!   connection, obtained from the `RemoteDesktop` portal on a desktop, or
//!   by connecting straight to gamescope's EIS socket in Steam Deck's
//!   Gaming Mode; the same code plays the events either way. On X11 the
//!   XTest extension (`x11_emulation.rs`).
//!
//! - **Clipboard**: on Wayland it rides on the emulation backend's
//!   RemoteDesktop session (`org.freedesktop.portal.Clipboard`), so it is
//!   there exactly when a peer can drive this desktop through the portal.
//!   On X11 it is the CLIPBOARD selection (`x11_clipboard.rs`), there for
//!   any program.
//!
//! Geometry is in logical points, the core's unit. The portals speak
//! logical pixels too; the X server speaks pixels, scaled in `x11_common.rs`.

mod clipboard_formats;
pub mod keymap;

#[cfg(target_os = "linux")]
mod ei_common;
#[cfg(target_os = "linux")]
pub mod ei_discover;
#[cfg(target_os = "linux")]
pub mod ei_emulation;
#[cfg(target_os = "linux")]
pub mod portal_capture;
#[cfg(target_os = "linux")]
pub mod portal_clipboard;
#[cfg(target_os = "linux")]
pub mod token;
#[cfg(target_os = "linux")]
pub mod x11_capture;
#[cfg(target_os = "linux")]
mod x11_clipboard;
#[cfg(target_os = "linux")]
mod x11_common;
#[cfg(target_os = "linux")]
pub mod x11_emulation;

#[cfg(target_os = "linux")]
pub use x11_common::X11Display;

use rinflow_core::layout::{Display, Side};

/// A line on one of this device's edges that the pointer may cross, in
/// logical points: `line` is the x (left/right) or y (top/bottom) of the
/// edge, `from..to` its extent along it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Barrier {
    /// The agent's own ID, reported back in [`CaptureEvent::Activated`].
    pub id: u32,
    pub side: Side,
    pub line: f64,
    pub from: f64,
    pub to: f64,
}

/// What the agent tells a capture backend.
#[derive(Debug)]
pub enum CaptureCommand {
    /// Replace the barriers (the portals on this device's edges changed).
    SetBarriers(Vec<Barrier>),
    /// The pointer that hit a barrier now drives a peer: the keyboard goes
    /// with it. The portal's capture takes both at the barrier; on X11 the
    /// keyboard is left to the desktop until this, so an edge hit that
    /// leads nowhere does not disturb the application in use.
    Engage,
    /// Give the pointer back to this desktop at this logical point; the
    /// capture ends.
    Release { x: f64, y: f64 },
    /// Give the pointer back whatever state the capture is in (the user's
    /// Ctrl+Alt+Esc while nothing is being driven): released if captured,
    /// and the session disabled and enabled again either way, which makes
    /// the compositor let go of anything it still holds.
    Escape,
    Shutdown,
}

/// What a capture backend reports.
#[derive(Debug, PartialEq)]
pub enum CaptureEvent {
    /// The backend is set up (and the user allowed it, where asked).
    Ready,
    /// The desktop is asking the user whether to allow it.
    Prompting,
    /// The displays as the compositor describes them, in logical points.
    Zones(Vec<Display>),
    /// The pointer hit barrier `barrier` at `(x, y)`; input now comes here.
    Activated { barrier: u32, x: f64, y: f64 },
    /// Relative pointer motion, logical points.
    Motion { dx: f64, dy: f64 },
    /// A pointer button by the core's number (1 left, 2 right, 3 middle…).
    Button { button: u32, down: bool },
    /// Scrolling in logical points, the core's signs (positive y toward the
    /// top of the content). `precise` for touchpads and smooth wheels.
    Scroll { dx: f64, dy: f64, precise: bool },
    /// A key by USB HID usage.
    Key { usage: u32, down: bool },
    /// The compositor ended the capture (after a release, or by itself).
    Deactivated,
    /// The user declined. Nothing will work until asked again.
    Denied(String),
    /// The session is gone (compositor restarted, EIS closed): the backend
    /// has to be started afresh.
    Lost(String),
}

/// Where an emulation backend gets its libei connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EisSource {
    /// `org.freedesktop.portal.RemoteDesktop`, with the user's permission.
    Portal,
    /// A compositor's own EIS socket, such as gamescope's `gamescope-0-ei`.
    Socket(std::path::PathBuf),
}

/// What an emulation backend reports.
#[derive(Debug, PartialEq)]
pub enum EmulationEvent {
    Ready,
    Prompting,
    Denied(String),
    Lost(String),
}

/// What the agent tells the clipboard of the emulation session.
#[derive(Debug)]
pub enum ClipboardCommand {
    /// A peer's clipboard becomes this desktop's, offered to applications
    /// until something here is copied.
    Offer(rinflow_core::Clipboard),
    /// Share this desktop's clipboard (read what is copied and report it),
    /// or stop: while off, nothing copied here is read at all. Off until
    /// told otherwise; turning it on reports the current selection.
    Enable(bool),
    /// Share copied files too (`text/uri-list`): while off, a file list on
    /// the clipboard is not read.
    Files(bool),
    /// The files offered from a peer are no longer there: if they are still
    /// what this session offers, the clipboard is emptied.
    WithdrawFiles,
}

/// What the clipboard of the emulation session reports.
#[derive(Debug, PartialEq)]
pub enum ClipboardEvent {
    /// The desktop gave this session its clipboard (`true`), or the session
    /// is over (`false`).
    Available(bool),
    /// Something was copied on this desktop: what of it is shared, most
    /// specific first (empty for a password manager's secret, or nothing
    /// Rinflow shares).
    Changed(rinflow_core::Clipboard),
}

/// The emulation backend's end of the clipboard channels.
pub struct ClipboardLink {
    pub commands: tokio::sync::mpsc::UnboundedReceiver<ClipboardCommand>,
    pub events: tokio::sync::mpsc::UnboundedSender<ClipboardEvent>,
}
