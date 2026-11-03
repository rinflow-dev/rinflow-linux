//! Which desktop this agent runs on, as far as input is concerned.
//!
//! An agent the desktop started (autostart, or the window) has the session's
//! variables in its own environment. One started by systemd as a user
//! service usually starts before any desktop, at login, and its environment
//! never changes; the desktop announces itself to the systemd user manager
//! instead (`systemctl --user import-environment`, which GNOME, Plasma and
//! Steam's gamescope-session all do). So both are read: the agent's own
//! variables first, then the manager's, and a Wayland display only counts if
//! its socket is there, so a desktop that has gone (a logout, the Steam Deck
//! switching modes) is not mistaken for the current one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Desktop {
    /// A Wayland session with a portal (GNOME, KDE, COSMIC…).
    Wayland,
    /// gamescope (Steam Deck's Gaming Mode): its own EIS socket, no capture.
    Gamescope,
    /// An X11 session: nothing yet.
    X11,
    /// No display at all.
    None,
}

/// A set of environment variables (the agent's, or the systemd manager's).
pub type Variables = HashMap<String, String>;

pub fn own_variables() -> Variables {
    std::env::vars().collect()
}

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait SystemdManager {
    #[zbus(property)]
    fn environment(&self) -> zbus::Result<Vec<String>>;
}

/// The systemd user manager's environment, which desktops update as they
/// start; `None` without a user manager (no systemd, a container).
pub async fn manager_variables(bus: &zbus::Connection) -> Option<Variables> {
    let proxy = SystemdManagerProxy::builder(bus)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .ok()?;
    let entries = proxy.environment().await.ok()?;
    Some(
        entries
            .iter()
            .filter_map(|entry| entry.split_once('='))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect(),
    )
}

/// Decides from the two environments. `socket_exists` checks a Wayland
/// socket; `gamescope_ei` tells whether gamescope's EIS socket is there.
pub fn decide(
    own: &Variables,
    manager: Option<&Variables>,
    runtime_directory: &Path,
    socket_exists: impl Fn(&Path) -> bool,
    gamescope_ei: bool,
) -> Desktop {
    let live = |variables: &Variables| {
        variables
            .get("WAYLAND_DISPLAY")
            .filter(|name| !name.is_empty())
            .is_some_and(|name| socket_exists(&wayland_socket(runtime_directory, name)))
    };
    let wayland = |variables: &Variables| {
        let gamescope = variables.contains_key("GAMESCOPE_WAYLAND_DISPLAY")
            || variables
                .get("WAYLAND_DISPLAY")
                .is_some_and(|name| name.starts_with("gamescope"));
        if gamescope { Desktop::Gamescope } else { Desktop::Wayland }
    };
    if live(own) {
        return wayland(own);
    }
    if let Some(manager) = manager.filter(|manager| live(manager)) {
        return wayland(manager);
    }
    // A display named without a socket where it should be: trust the
    // agent's own environment as it always did (a nested or test session).
    if own.get("WAYLAND_DISPLAY").is_some_and(|name| !name.is_empty()) {
        return wayland(own);
    }
    if gamescope_ei {
        return Desktop::Gamescope;
    }
    let x11 = |variables: &Variables| variables.get("DISPLAY").is_some_and(|d| !d.is_empty());
    if x11(own) || manager.is_some_and(x11) {
        return Desktop::X11;
    }
    Desktop::None
}

fn wayland_socket(runtime_directory: &Path, name: &str) -> PathBuf {
    let path = Path::new(name);
    if path.is_absolute() {
        path.to_owned()
    } else {
        runtime_directory.join(path)
    }
}

pub fn runtime_directory() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // SAFETY: getuid has no preconditions.
            let uid = unsafe { libc::getuid() };
            PathBuf::from(format!("/run/user/{uid}"))
        })
}

pub fn is_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.file_type().is_socket())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Variables {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    const RUN: &str = "/run/user/1000";

    fn sockets(names: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |path| names.iter().any(|name| path == Path::new(RUN).join(name))
    }

    #[test]
    fn a_systemd_service_finds_the_desktop_that_came_after_it() {
        // Started at login, before GNOME: nothing in its own environment.
        let own = vars(&[("HOME", "/home/u")]);
        let manager = vars(&[("WAYLAND_DISPLAY", "wayland-0"), ("XDG_CURRENT_DESKTOP", "GNOME")]);
        let run = Path::new(RUN);
        assert_eq!(decide(&own, None, run, sockets(&[]), false), Desktop::None);
        assert_eq!(decide(&own, Some(&manager), run, sockets(&["wayland-0"]), false), Desktop::Wayland);
        // The manager still names it after a logout; the socket is gone.
        assert_eq!(decide(&own, Some(&manager), run, sockets(&[]), false), Desktop::None);
    }

    #[test]
    fn the_steam_deck_switching_modes_is_followed() {
        let run = Path::new(RUN);
        // Started in Desktop Mode (Plasma): its own environment says wayland-0.
        let own = vars(&[("WAYLAND_DISPLAY", "wayland-0")]);
        let gaming = vars(&[("WAYLAND_DISPLAY", "gamescope-0"), ("GAMESCOPE_WAYLAND_DISPLAY", "gamescope-0")]);
        assert_eq!(decide(&own, None, run, sockets(&["wayland-0"]), false), Desktop::Wayland);
        // Switched to Gaming Mode: Plasma's socket is gone, gamescope's is up.
        assert_eq!(decide(&own, Some(&gaming), run, sockets(&["gamescope-0"]), true), Desktop::Gamescope);
    }

    #[test]
    fn the_agents_own_display_wins_and_x11_is_recognised() {
        let run = Path::new(RUN);
        let own = vars(&[("WAYLAND_DISPLAY", "wayland-1")]);
        let manager = vars(&[("WAYLAND_DISPLAY", "wayland-0")]);
        assert_eq!(decide(&own, Some(&manager), run, sockets(&["wayland-0", "wayland-1"]), false), Desktop::Wayland);
        // No socket where it should be (a test or nested session): still trusted.
        assert_eq!(decide(&own, None, run, sockets(&[]), false), Desktop::Wayland);
        assert_eq!(decide(&vars(&[]), Some(&vars(&[("DISPLAY", ":0")])), run, sockets(&[]), false), Desktop::X11);
        assert_eq!(
            decide(&vars(&[("WAYLAND_DISPLAY", "/tmp/w")]), None, run, |p| p == Path::new("/tmp/w"), false),
            Desktop::Wayland
        );
    }
}
