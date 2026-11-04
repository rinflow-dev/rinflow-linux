//! The agent's socket, from the window's side: connects (starting the agent
//! if nothing answers), reads snapshots, sends requests. Everything runs on
//! the main context; callbacks are plain closures.

use gtk::glib;
use gtk::prelude::*;
use gtk::gio;
use rinflow_core::layout::Offset;
use rinflow_shell::protocol::PROTOCOL_VERSION;
use rinflow_shell::state::Snapshot;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Incoming {
    Hello { protocol: u32, agent_version: String },
    State(Snapshot),
    Error { message: String },
    PlacementPreview { peer_id: String, offset: Option<Offset> },
}

/// How the window stands with the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Connected,
    /// The agent could not be started or reached; the text says why.
    Unreachable(String),
}

type SnapshotHandler = Box<dyn Fn(&Snapshot)>;
type ErrorHandler = Box<dyn Fn(&str)>;
type PreviewHandler = Box<dyn Fn(&str, Option<Offset>)>;
type StatusHandler = Box<dyn Fn(&Status)>;

#[derive(Default)]
struct Inner {
    output: RefCell<Option<gio::OutputStream>>,
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
                            self.set_status(Status::Unreachable(rinflow_shell::i18n::fill(
                                &gettextrs::gettext("The agent couldn't be started: {error}"),
                                &[("error", &error)],
                            )));
                        }
                    } else if attempts > 20 {
                        self.set_status(Status::Unreachable(rinflow_shell::i18n::fill(
                            &gettextrs::gettext("The agent isn't answering ({error})"),
                            &[("error", &error)],
                        )));
                    }
                    glib::timeout_future(Duration::from_millis(if attempts > 20 { 2000 } else { 400 }))
                        .await;
                }
            }
        }
    }

    async fn serve(&self, connection: gio::SocketConnection) {
        let input = gio::DataInputStream::new(&connection.input_stream());
        *self.0.output.borrow_mut() = Some(connection.output_stream());
        self.send(json!({"type": "hello", "protocol": PROTOCOL_VERSION}));
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
                        self.report_error(&rinflow_shell::i18n::fill(
                            &gettextrs::gettext("The agent and this window are different versions; update both ({version} / {protocol})"),
                            &[("version", &agent_version), ("protocol", &protocol)],
                        ));
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

    fn report_error(&self, message: &str) {
        for handler in self.0.on_error.borrow().iter() {
            handler(message);
        }
    }

    fn send(&self, request: Value) {
        let Some(output) = self.0.output.borrow().clone() else {
            log::warn!("not connected to the agent; dropped {request}");
            return;
        };
        let mut line = request.to_string();
        line.push('\n');
        glib::spawn_future_local(async move {
            if let Err(error) = output
                .write_all_future(line.into_bytes(), glib::Priority::DEFAULT)
                .await
            {
                log::warn!("cannot write to the agent: {:?}", error.1);
            }
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

/// Starts the agent next to this binary (or from `PATH`), detached, so it
/// outlives the window.
fn spawn_agent() -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("rinflow-agent")))
        .filter(|path| path.is_file());
    let program = beside.unwrap_or_else(|| PathBuf::from("rinflow-agent"));
    std::process::Command::new(program)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
}
