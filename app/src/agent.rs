//! The agent's socket, from the window's side: connects (starting the agent
//! if nothing answers), reads snapshots, sends requests. Everything runs on
//! the main context; callbacks are plain closures.

use gtk::glib;
use gtk::prelude::*;
use gtk::gio;
use rinflow_core::layout::Offset;
use rinflow_shell::i18n::{Text, msg};
use rinflow_shell::protocol::PROTOCOL_VERSION;
use rinflow_shell::state::Snapshot;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Duration;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Incoming {
    Hello { protocol: u32, agent_version: String },
    State(Snapshot),
    Error { message: Text },
    PlacementPreview { peer_id: String, offset: Option<Offset> },
}

/// How the window stands with the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Connected,
    /// The agent could not be started or reached; the text says why.
    Unreachable(Text),
}

type SnapshotHandler = Box<dyn Fn(&Snapshot)>;
type ErrorHandler = Box<dyn Fn(&str)>;
type PreviewHandler = Box<dyn Fn(&str, Option<Offset>)>;
type StatusHandler = Box<dyn Fn(&Status)>;

#[derive(Default)]
struct Inner {
    output: RefCell<Option<gio::OutputStream>>,
    /// Lines waiting to be written. A GIO stream takes one write at a time
    /// (another one meanwhile fails with "outstanding operation" and the
    /// request is lost), so they go out in order from one writer.
    outgoing: RefCell<VecDeque<String>>,
    writing: Cell<bool>,
    snapshot: RefCell<Option<Snapshot>>,
    status: RefCell<Option<Status>>,
    on_snapshot: RefCell<Vec<SnapshotHandler>>,
    on_error: RefCell<Vec<ErrorHandler>>,
    on_preview: RefCell<Vec<PreviewHandler>>,
    on_status: RefCell<Vec<StatusHandler>>,
    /// Whether this window asked the agent to quit: no restart then.
    quitting: RefCell<bool>,
}

#[derive(Clone, Default)]
pub struct AgentClient(Rc<Inner>);

impl AgentClient {
    pub fn connect_snapshot(&self, handler: impl Fn(&Snapshot) + 'static) {
        self.0.on_snapshot.borrow_mut().push(Box::new(handler));
    }

    pub fn connect_error(&self, handler: impl Fn(&str) + 'static) {
        self.0.on_error.borrow_mut().push(Box::new(handler));
    }

    pub fn connect_preview(&self, handler: impl Fn(&str, Option<Offset>) + 'static) {
        self.0.on_preview.borrow_mut().push(Box::new(handler));
    }

    pub fn connect_status(&self, handler: impl Fn(&Status) + 'static) {
        self.0.on_status.borrow_mut().push(Box::new(handler));
    }

    pub fn snapshot(&self) -> Option<Snapshot> {
        self.0.snapshot.borrow().clone()
    }

    /// Connects, and keeps connecting for as long as the window lives.
    pub fn start(&self) {
        let client = self.clone();
        glib::spawn_future_local(async move {
            client.run().await;
        });
    }

    async fn run(&self) {
        let path = socket_path();
        lend_session_to_systemd();
        let mut spawned = false;
        let mut attempts = 0u32;
        loop {
            if *self.0.quitting.borrow() {
                return;
            }
            self.set_status(Status::Connecting);
            match connect(&path).await {
                Ok(connection) => {
                    attempts = 0;
                    spawned = false;
                    self.set_status(Status::Connected);
                    self.serve(connection).await;
                    *self.0.output.borrow_mut() = None;
                    if self
                        .0
                        .snapshot
                        .borrow()
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.quitting)
                    {
                        return;
                    }
                    glib::timeout_future(Duration::from_millis(300)).await;
                }
                Err(error) => {
                    attempts += 1;
                    if !spawned {
                        spawned = true;
                        if let Err(error) = spawn_agent() {
                            self.set_status(Status::Unreachable(
                                msg("The agent couldn't be started: {error}").arg("error", error.to_string()),
                            ));
                        }
                    } else if attempts > 20 {
                        self.set_status(Status::Unreachable(
                            msg("The agent isn't answering ({error})").arg("error", error.to_string()),
                        ));
                    }
                    glib::timeout_future(Duration::from_millis(if attempts > 20 { 2000 } else { 400 }))
                        .await;
                }
            }
        }
    }

    async fn serve(&self, connection: gio::SocketConnection) {
        let input = gio::DataInputStream::new(&connection.input_stream());
        // Whatever was meant for the last connection doesn't go to this one.
        self.0.outgoing.borrow_mut().clear();
        *self.0.output.borrow_mut() = Some(connection.output_stream());
        // "window": the agent leaves asking the user (a pairing, say) to
        // this window instead of sending a notification.
        self.send(json!({"type": "hello", "protocol": PROTOCOL_VERSION, "client": "window"}));
        loop {
            let line = match input.read_line_utf8_future(glib::Priority::DEFAULT).await {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    log::warn!("agent connection: {error}");
                    break;
                }
            };
            match serde_json::from_str::<Incoming>(&line) {
                Ok(Incoming::Hello { protocol, agent_version }) => {
                    if protocol != PROTOCOL_VERSION {
                        self.report_error(
                            &msg("The agent and this window are different versions; update both ({version} / {protocol})")
                                .arg("version", agent_version)
                                .arg("protocol", protocol.to_string()),
                        );
                    }
                }
                Ok(Incoming::State(snapshot)) => {
                    let quitting = snapshot.quitting;
                    *self.0.snapshot.borrow_mut() = Some(snapshot);
                    let snapshot = self.0.snapshot.borrow();
                    if let Some(snapshot) = snapshot.as_ref() {
                        for handler in self.0.on_snapshot.borrow().iter() {
                            handler(snapshot);
                        }
                    }
                    if quitting {
                        break;
                    }
                }
                Ok(Incoming::Error { message }) => self.report_error(&message),
                Ok(Incoming::PlacementPreview { peer_id, offset }) => {
                    for handler in self.0.on_preview.borrow().iter() {
                        handler(&peer_id, offset);
                    }
                }
                Err(error) => log::warn!("unreadable message from the agent: {error}: {line}"),
            }
        }
    }

    fn set_status(&self, status: Status) {
        if self.0.status.borrow().as_ref() == Some(&status) {
            return;
        }
        *self.0.status.borrow_mut() = Some(status.clone());
        for handler in self.0.on_status.borrow().iter() {
            handler(&status);
        }
    }

    fn report_error(&self, message: &Text) {
        let message = message.translate();
        for handler in self.0.on_error.borrow().iter() {
            handler(&message);
        }
    }

    /// How the window stands with the agent now.
    pub fn status(&self) -> Option<Status> {
        self.0.status.borrow().clone()
    }

    fn send(&self, request: Value) {
        if self.0.output.borrow().is_none() {
            log::warn!("not connected to the agent; dropped {request}");
            return;
        }
        let mut line = request.to_string();
        line.push('\n');
        self.0.outgoing.borrow_mut().push_back(line);
        if self.0.writing.replace(true) {
            return;
        }
        let client = self.clone();
        glib::spawn_future_local(async move {
            loop {
                let Some(line) = client.0.outgoing.borrow_mut().pop_front() else { break };
                let Some(output) = client.0.output.borrow().clone() else { break };
                if let Err(error) = output
                    .write_all_future(line.into_bytes(), glib::Priority::DEFAULT)
                    .await
                {
                    log::warn!("cannot write to the agent: {:?}", error.1);
                }
            }
            client.0.writing.set(false);
        });
    }

    pub fn connect_peer(&self, peer_id: &str) {
        self.send(json!({"type": "connect", "peer_id": peer_id}));
    }

    pub fn disconnect_peer(&self, peer_id: &str) {
        self.send(json!({"type": "disconnect", "peer_id": peer_id}));
    }

    pub fn set_placement(&self, peer_id: &str, offset: Offset) {
        self.send(json!({"type": "set_placement", "peer_id": peer_id, "x": offset.x, "y": offset.y}));
    }

    pub fn remove_placement(&self, peer_id: &str) {
        self.send(json!({"type": "remove_placement", "peer_id": peer_id}));
    }

    pub fn preview_placement(&self, peer_id: &str, offset: Offset) {
        self.send(json!({"type": "preview_placement", "peer_id": peer_id, "x": offset.x, "y": offset.y}));
    }

    pub fn place_beside(&self, peer_id: &str, side: &str) {
        self.send(json!({"type": "place_beside", "peer_id": peer_id, "side": side}));
    }

    pub fn update_settings(&self, values: Map<String, Value>) {
        self.send(json!({"type": "update_settings", "values": values}));
    }

    pub fn update_setting(&self, key: &str, value: Value) {
        let mut values = Map::new();
        values.insert(key.to_owned(), value);
        self.update_settings(values);
    }

    pub fn forget_device(&self, peer_id: &str) {
        self.send(json!({"type": "forget_device", "peer_id": peer_id}));
    }

    pub fn accept_pairing(&self, peer_id: &str) {
        self.send(json!({"type": "accept_pairing", "peer_id": peer_id}));
    }

    pub fn reject_pairing(&self, peer_id: &str) {
        self.send(json!({"type": "reject_pairing", "peer_id": peer_id}));
    }

    pub fn clear_error(&self) {
        self.send(json!({"type": "clear_error"}));
    }

    pub fn set_locked(&self, locked: bool) {
        self.send(json!({"type": "set_locked", "locked": locked}));
    }

    pub fn cancel_files(&self, transfer: u32) {
        self.send(json!({"type": "cancel_files", "transfer": transfer}));
    }

    pub fn retry_input(&self) {
        self.send(json!({"type": "retry_input"}));
    }

    pub fn quit_agent(&self) {
        *self.0.quitting.borrow_mut() = true;
        self.send(json!({"type": "quit"}));
    }
}

async fn connect(path: &PathBuf) -> Result<gio::SocketConnection, glib::Error> {
    let address = gio::UnixSocketAddress::new(path);
    gio::SocketClient::new()
        .connect_future(&address)
        .await
}

/// `$XDG_RUNTIME_DIR/rinflow/agent.sock`, as the agent's `paths.rs` has it.
fn socket_path() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // SAFETY: getuid has no preconditions.
            let uid = unsafe { libc::getuid() };
            PathBuf::from(format!("/run/user/{uid}"))
        });
    runtime.join("rinflow").join("agent.sock")
}

/// The user's `LANGUAGE`, as it was before the window set its own for GTK.
static LANGUAGE_LIST: OnceLock<Option<String>> = OnceLock::new();

/// Remembers the user's `LANGUAGE` (see `main`), so the agent this window
/// starts gets the user's environment rather than the window's.
pub fn keep_language_list(list: Option<String>) {
    let _ = LANGUAGE_LIST.set(list);
}

/// The systemd user unit installed with the agent.
const AGENT_UNIT: &str = "rinflow-agent.service";

/// The variables that say which desktop a process is on, and how to
/// reach it.
const SESSION_VARIABLES: [&str; 6] = ["DISPLAY", "XAUTHORITY", "WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE", "DBUS_SESSION_BUS_ADDRESS"];

/// Tells the systemd user manager which desktop this window is on, when
/// the manager knows of none. The agent is a user service and learns of
/// the desktop from the manager. GNOME, Plasma and distributions' X
/// session scripts tell it as they start; a session started by hand
/// (`startx` with a window manager), or one without the user bus, does
/// not, and the agent would say there is no desktop to the very window
/// that is on it. A manager that knows a display is left alone: it may be
/// another session's, and what it knows is that session's to say.
fn lend_session_to_systemd() {
    let systemctl = |args: &[&str]| {
        std::process::Command::new("systemctl")
            .arg("--user")
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
    };
    // Only where the agent is the installed service: one started from a
    // source tree or a sandbox is this window's child, and has its
    // environment (and there may be no systemd at all).
    let installed = systemctl(&["show", "--property=LoadState", "--value", AGENT_UNIT]).is_some_and(|shown| String::from_utf8_lossy(&shown.stdout).trim() == "loaded");
    if !installed {
        return;
    }
    let Some(shown) = systemctl(&["show-environment"]) else { return };
    let ours = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let missing = session_variables_to_lend(&String::from_utf8_lossy(&shown.stdout), ours);
    if missing.is_empty() {
        return;
    }
    let mut arguments = vec!["import-environment"];
    arguments.extend(&missing);
    if systemctl(&arguments).is_some() {
        log::info!("told the systemd user manager about this session: {missing:?}");
    }
}

/// The session variables to hand to a manager whose environment is
/// `known` (`NAME=value` lines): those set here (`ours`) that it lacks,
/// and none at all when it already names a display, or this process has
/// none to name.
fn session_variables_to_lend(known: &str, ours: impl Fn(&str) -> bool) -> Vec<&'static str> {
    let knows = |name: &str| known.lines().any(|line| line.strip_prefix(name).is_some_and(|rest| rest.starts_with('=')));
    let display = |has: &dyn Fn(&str) -> bool| has("DISPLAY") || has("WAYLAND_DISPLAY");
    if display(&knows) || !display(&ours) {
        return Vec::new();
    }
    SESSION_VARIABLES.into_iter().filter(|name| ours(name) && !knows(name)).collect()
}

/// Starts the agent next to this binary (or from `PATH`), detached, so it
/// outlives the window. When the user service runs that same program, it
/// is started through systemd instead: an agent started here would beat
/// systemd's restart after a crash, and live outside the service.
fn spawn_agent() -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("rinflow-agent")))
        .filter(|path| path.is_file());
    let program = beside.unwrap_or_else(|| PathBuf::from("rinflow-agent"));
    if start_through_systemd(&program) {
        return Ok(());
    }
    let mut command = std::process::Command::new(program);
    match LANGUAGE_LIST.get().cloned().flatten() {
        Some(list) => command.env("LANGUAGE", list),
        None => command.env_remove("LANGUAGE"),
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map(|mut child| {
            // Reaped when it exits, so it does not linger as a zombie under
            // a window that stays open.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        })
}

/// Starts the user service if it is installed and runs `program` (a build
/// started from a source tree runs its own agent instead). False when it
/// is not, or systemd is not there (Flatpak, no user manager).
fn start_through_systemd(program: &std::path::Path) -> bool {
    let systemctl = |args: &[&str]| {
        std::process::Command::new("systemctl")
            .arg("--user")
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
    };
    // `{ path=/usr/bin/rinflow-agent ; argv[]=… }`, or empty when the unit
    // is not installed.
    let Some(show) = systemctl(&["show", "--property=ExecStart", "--value", AGENT_UNIT]) else {
        return false;
    };
    let show = String::from_utf8_lossy(&show.stdout);
    let Some(unit_program) = show
        .split("path=")
        .nth(1)
        .and_then(|rest| rest.split(" ;").next())
        .map(|path| PathBuf::from(path.trim()))
    else {
        return false;
    };
    let same = |a: &std::path::Path, b: &std::path::Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if !same(&unit_program, program) {
        return false;
    }
    let started = systemctl(&["start", "--no-block", AGENT_UNIT]).is_some();
    if started {
        log::info!("started {AGENT_UNIT}");
    }
    started
}

#[cfg(test)]
mod tests {
    use super::session_variables_to_lend;

    #[test]
    fn a_manager_without_a_display_is_told_of_this_one() {
        let x11 = |name: &str| matches!(name, "DISPLAY" | "XAUTHORITY" | "XDG_SESSION_TYPE" | "DBUS_SESSION_BUS_ADDRESS");
        // A session started by hand: the manager knows nothing of it.
        assert_eq!(
            session_variables_to_lend("HOME=/home/u\nLANG=C.UTF-8\n", x11),
            ["DISPLAY", "XAUTHORITY", "XDG_SESSION_TYPE", "DBUS_SESSION_BUS_ADDRESS"]
        );
        // What it has already is its own.
        assert_eq!(
            session_variables_to_lend("DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus\nXDG_SESSION_TYPE=tty\n", x11),
            ["DISPLAY", "XAUTHORITY"]
        );
        // A desktop that told the manager itself, or another session's
        // display: nothing is added to it.
        assert!(session_variables_to_lend("DISPLAY=:0\n", x11).is_empty());
        assert!(session_variables_to_lend("WAYLAND_DISPLAY=wayland-0\nXDG_SESSION_TYPE=wayland\n", x11).is_empty());
        // DISPLAYMANAGER is not DISPLAY.
        assert_eq!(session_variables_to_lend("DISPLAYMANAGER=lightdm\n", x11).len(), 4);
        // Nothing to tell from a process without a display.
        assert!(session_variables_to_lend("", |name| name == "XDG_SESSION_TYPE").is_empty());
    }
}
