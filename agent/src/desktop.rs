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
//!
//! An X11 session is one with `DISPLAY` and no Wayland display (a Wayland
//! session has `DISPLAY` too, for its X programs). The manager's is taken
//! only while that server's socket is there, for the same reason.

use rinflow_input::X11Display;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Desktop {
    /// A Wayland session with a portal (GNOME, KDE, COSMIC…).
    Wayland,
    /// gamescope (Steam Deck's Gaming Mode): its own EIS socket, no capture.
    Gamescope,
    /// An X11 session (Cinnamon, Xfce, MATE, a window manager by itself…):
    /// the X server's own extensions. Which server: [`x11_display`].
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
    if x11_display(own, manager, socket_exists).is_some() {
        return Desktop::X11;
    }
    Desktop::None
}

/// The X server of the session, with the authority file named beside it:
/// the agent's own `DISPLAY` as it is, else the manager's while its socket
/// is there. Only meaningful when [`decide`] says [`Desktop::X11`].
pub fn x11_display(own: &Variables, manager: Option<&Variables>, socket_exists: impl Fn(&Path) -> bool) -> Option<X11Display> {
    let named = |variables: &Variables| {
        // A Wayland session whose display was not found: its X server is
        // only there for X programs, and is not the desktop.
        if variables.get("XDG_SESSION_TYPE").is_some_and(|kind| kind == "wayland") {
            return None;
        }
        let name = variables.get("DISPLAY").filter(|name| !name.is_empty())?;
        let authority = variables.get("XAUTHORITY").filter(|path| !path.is_empty()).map(PathBuf::from);
        Some(X11Display { name: name.clone(), authority })
    };
    named(own).or_else(|| {
        named(manager?).filter(|display| x11_socket(&display.name).is_none_or(|socket| socket_exists(&socket)))
    })
}

/// Where a local X server listens (`:0`, `:1.0`, `unix/:0`); `None` for
/// one reached over the network, which cannot be looked at.
fn x11_socket(display: &str) -> Option<PathBuf> {
    let local = display.strip_prefix("unix/").or_else(|| display.strip_prefix("unix")).unwrap_or(display);
    let number = local.strip_prefix(':')?.split('.').next()?;
    (!number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())).then(|| PathBuf::from(format!("/tmp/.X11-unix/X{number}")))
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
    fn the_agents_own_display_wins() {
        let run = Path::new(RUN);
        let own = vars(&[("WAYLAND_DISPLAY", "wayland-1")]);
        let manager = vars(&[("WAYLAND_DISPLAY", "wayland-0")]);
        assert_eq!(decide(&own, Some(&manager), run, sockets(&["wayland-0", "wayland-1"]), false), Desktop::Wayland);
        // No socket where it should be (a test or nested session): still trusted.
        assert_eq!(decide(&own, None, run, sockets(&[]), false), Desktop::Wayland);
        assert_eq!(
            decide(&vars(&[("WAYLAND_DISPLAY", "/tmp/w")]), None, run, |p| p == Path::new("/tmp/w"), false),
            Desktop::Wayland
        );
    }

    #[test]
    fn an_x11_session_is_found_with_its_server_and_authority() {
        let run = Path::new(RUN);
        let x0 = |path: &Path| path == Path::new("/tmp/.X11-unix/X0");
        // Started by the session: its own variables, as they are.
        let own = vars(&[("DISPLAY", ":1"), ("XAUTHORITY", "/home/u/.Xauthority")]);
        assert_eq!(decide(&own, None, run, sockets(&[]), false), Desktop::X11);
        assert_eq!(
            x11_display(&own, None, sockets(&[])),
            Some(X11Display { name: ":1".to_owned(), authority: Some(PathBuf::from("/home/u/.Xauthority")) })
        );
        // Started at login, before the session: the manager's, while the
        // server is there.
        let manager = vars(&[("DISPLAY", ":0"), ("XAUTHORITY", "/run/user/1000/xauth")]);
        assert_eq!(decide(&vars(&[]), Some(&manager), run, x0, false), Desktop::X11);
        assert_eq!(x11_display(&vars(&[]), Some(&manager), x0).unwrap().authority, Some(PathBuf::from("/run/user/1000/xauth")));
        // After a logout the manager still names it; the socket is gone.
        assert_eq!(decide(&vars(&[]), Some(&manager), run, sockets(&[]), false), Desktop::None);
        // A Wayland session has DISPLAY too, for its X programs.
        let both = vars(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")]);
        assert_eq!(decide(&both, None, run, sockets(&["wayland-0"]), false), Desktop::Wayland);
        // ... and is no X11 session even when its Wayland display is not known.
        let lost = vars(&[("XDG_SESSION_TYPE", "wayland"), ("DISPLAY", ":0")]);
        assert_eq!(decide(&lost, None, run, x0, false), Desktop::None);
        // A server on another machine cannot be looked at: taken as named.
        let remote = vars(&[("DISPLAY", "host:10.0")]);
        assert_eq!(decide(&vars(&[]), Some(&remote), run, sockets(&[]), false), Desktop::X11);
    }

    #[test]
    fn local_x_servers_have_a_socket_to_look_for() {
        assert_eq!(x11_socket(":0"), Some(PathBuf::from("/tmp/.X11-unix/X0")));
        assert_eq!(x11_socket(":12.0"), Some(PathBuf::from("/tmp/.X11-unix/X12")));
        assert_eq!(x11_socket("unix/:3"), Some(PathBuf::from("/tmp/.X11-unix/X3")));
        assert_eq!(x11_socket("unix:3"), Some(PathBuf::from("/tmp/.X11-unix/X3")));
        assert_eq!(x11_socket("host:0"), None);
        assert_eq!(x11_socket(":"), None);
    }
}
