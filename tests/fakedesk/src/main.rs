//! A desktop for tests. It speaks the two portals the agent uses, over a
//! session bus of its own, and the libei wire protocol through `reis`'s
//! server side; it never shows a dialog. An orchestrator drives it over a
//! control socket (JSON lines): make the pointer hit a barrier, move it,
//! press keys; ask what the agent on the other side played.
//!
//!   fakedesk --control /tmp/fd.sock [--zones 1920x1080[+0+0],...]
//!
//! Commands: {"cmd":"state"}, {"cmd":"activate","barrier":1,"x":1920,"y":500},
//! {"cmd":"motion","dx":10,"dy":0}, {"cmd":"key","code":30,"down":true},
//! {"cmd":"button","code":272,"down":true}, {"cmd":"scroll","dx":0,"dy":15}.
//!
//! The clipboard (`org.freedesktop.portal.Clipboard` on the RemoteDesktop
//! session): {"cmd":"copy","formats":[["text/plain", "<hex>"], …]} is an
//! application here copying; {"cmd":"paste","mime":"text/plain"} is one
//! pasting what the agent offers, answered in `pastes` of the state.
//!
//! With `--systemd` it also stands in for the systemd user manager, whose
//! environment a desktop updates as it starts:
//! {"cmd":"systemd_env","env":["WAYLAND_DISPLAY=wayland-1", …]}.

use reis::eis;
use reis::handshake::EisHandshaker;
use reis::request::{DeviceCapability, EisRequest, EisRequestConverter};
use serde::Serialize;
use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use zbus::message::Header;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, SerializeDict, Type};
use zbus::{Connection, interface};

const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
const RESTORE_TOKEN: &str = "fakedesk-restore-token";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// The agent captures: we are the EIS sender, it receives.
    Capture,
    /// The agent plays input: it sends, we record.
    Emulation,
}

#[derive(Debug)]
enum Inject {
    Motion(f64, f64),
    Key(u32, bool),
    Button(u32, bool),
    Scroll(f64, f64),
}

#[derive(Default, Serialize)]
struct Snapshot {
    zones: Vec<(u32, u32, i32, i32)>,
    barriers: Vec<(u32, i32, i32, i32, i32)>,
    barrier_sets: u32,
    /// SetPointerBarriers calls refused because the session was enabled.
    refused_sets: u32,
    enabled: bool,
    enables: u32,
    disables: u32,
    capture_session: Option<String>,
    emulation_session: Option<String>,
    capture_eis: bool,
    emulation_eis: bool,
    releases: Vec<(Option<u32>, Option<(f64, f64)>)>,
    received: Vec<serde_json::Value>,
    restore_token_seen: Option<String>,
    /// The agent asked for the clipboard with its RemoteDesktop session.
    clipboard_requested: bool,
    /// What the agent last announced with SetSelection, and how often.
    offered_types: Vec<String>,
    selection_sets: u32,
    /// SelectionRead calls, by MIME type.
    reads: Vec<String>,
    /// Pastes by serial: MIME type, what the agent wrote (hex), and its
    /// SelectionWriteDone.
    pastes: HashMap<u32, Paste>,
}

#[derive(Default, Serialize, Clone)]
struct Paste {
    mime: String,
    data: String,
    done: Option<bool>,
}

struct State {
    snapshot: Snapshot,
    activation: u32,
    zone_set: u32,
    receiver: Option<mpsc::UnboundedSender<Inject>>,
    new_eis: mpsc::UnboundedSender<(Kind, UnixStream)>,
    /// What applications here copied: (MIME type, bytes).
    desktop_clipboard: Vec<(String, Vec<u8>)>,
    transfer_serial: u32,
    /// The systemd user manager's `Environment` (with `--systemd`).
    systemd_env: Vec<String>,
}

struct SystemdManager(Shared);

#[interface(name = "org.freedesktop.systemd1.Manager")]
impl SystemdManager {
    #[zbus(property)]
    fn environment(&self) -> Vec<String> {
        self.0.lock().unwrap().systemd_env.clone()
    }
}

type Shared = Arc<Mutex<State>>;

fn request_path(header: &Header<'_>, options: &HashMap<String, OwnedValue>, key: &str) -> OwnedObjectPath {
    let sender = header
        .sender()
        .map(|s| s.as_str().trim_start_matches(':').replace('.', "_"))
        .unwrap_or_else(|| "unknown".into());
    let token = options
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or("t");
    let base = if key == "handle_token" { "request" } else { "session" };
    OwnedObjectPath::try_from(format!("{DESKTOP_PATH}/{base}/{sender}/{token}")).expect("object path")
}

/// Answers a request a moment after the method itself returned, as a real
/// portal does; `results` is the `a{sv}` of the `Response` signal.
fn respond<R>(connection: &Connection, request: OwnedObjectPath, results: R)
where
    R: Serialize + Type + Send + Sync + 'static,
{
    let connection = connection.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        if let Err(error) = connection
            .emit_signal(
                None::<zbus::names::BusName<'_>>,
                &request,
                "org.freedesktop.portal.Request",
                "Response",
                &(0u32, results),
            )
            .await
        {
            log::warn!("Response on {request}: {error}");
        }
    });
}

async fn register_session(connection: &Connection, path: &OwnedObjectPath) {
    let _ = connection
        .object_server()
        .at(path.as_ref(), SessionIface)
        .await;
}

struct SessionIface;

#[interface(name = "org.freedesktop.portal.Session")]
impl SessionIface {
    async fn close(&self) {}

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

#[derive(SerializeDict, Type)]
#[zvariant(signature = "dict")]
struct CreateSessionResults {
    session_handle: OwnedObjectPath,
    capabilities: u32,
}

#[derive(SerializeDict, Type)]
#[zvariant(signature = "dict")]
struct SessionOnlyResults {
    session_handle: OwnedObjectPath,
}

#[derive(SerializeDict, Type)]
#[zvariant(signature = "dict")]
struct ZonesResults {
    zones: Vec<(u32, u32, i32, i32)>,
    zone_set: u32,
}

#[derive(SerializeDict, Type)]
#[zvariant(signature = "dict")]
struct BarriersResults {
    failed_barriers: Vec<u32>,
}

#[derive(SerializeDict, Type)]
#[zvariant(signature = "dict")]
struct Empty {}

#[derive(SerializeDict, Type)]
#[zvariant(signature = "dict")]
struct StartResults {
    devices: u32,
    restore_token: String,
    clipboard_enabled: bool,
}

#[derive(SerializeDict, Type)]
#[zvariant(signature = "dict")]
struct OwnerChanged {
    mime_types: Vec<String>,
    session_is_owner: bool,
}

/// As xdg-desktop-portal 1.20 really sends it: the types wrapped in a
/// struct, `(as)`, where the specification has `as`.
#[derive(SerializeDict, Type)]
#[zvariant(signature = "dict")]
struct OwnerChangedAsSent {
    mime_types: (Vec<String>,),
    session_is_owner: bool,
}

#[derive(SerializeDict, Type)]
#[zvariant(signature = "dict")]
struct ActivatedOptions {
    activation_id: u32,
    cursor_position: (f64, f64),
    barrier_id: u32,
}

#[derive(SerializeDict, Type)]
#[zvariant(signature = "dict")]
struct DeactivatedOptions {
    activation_id: u32,
}

struct InputCaptureIface(Shared);

#[interface(name = "org.freedesktop.portal.InputCapture")]
impl InputCaptureIface {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }

    #[zbus(property)]
    fn supported_capabilities(&self) -> u32 {
        1 | 2
    }

    async fn create_session(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        _parent_window: &str,
        options: HashMap<String, OwnedValue>,
    ) -> OwnedObjectPath {
        let request = request_path(&header, &options, "handle_token");
        let session = request_path(&header, &options, "session_handle_token");
        register_session(connection, &session).await;
        self.0.lock().unwrap().snapshot.capture_session = Some(session.to_string());
        log::info!("InputCapture.CreateSession → {session}");
        respond(connection, request.clone(), CreateSessionResults { session_handle: session, capabilities: 3 });
        request
    }

    async fn get_zones(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        _session: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
    ) -> OwnedObjectPath {
        let request = request_path(&header, &options, "handle_token");
        let (zones, zone_set) = {
            let state = self.0.lock().unwrap();
            (state.snapshot.zones.clone(), state.zone_set)
        };
        respond(connection, request.clone(), ZonesResults { zones, zone_set });
        request
    }

    async fn set_pointer_barriers(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        _session: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
        barriers: Vec<HashMap<String, OwnedValue>>,
        _zone_set: u32,
    ) -> OwnedObjectPath {
        let request = request_path(&header, &options, "handle_token");
        let mut parsed = Vec::new();
        for barrier in &barriers {
            let id = barrier.get("barrier_id").and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
            let (x1, y1, x2, y2) = barrier
                .get("position")
                .and_then(|v| <(i32, i32, i32, i32)>::try_from(v.clone()).ok())
                .unwrap_or_default();
            parsed.push((id, x1, y1, x2, y2));
        }
        log::info!("InputCapture.SetPointerBarriers {parsed:?}");
        let failed_barriers = {
            let mut state = self.0.lock().unwrap();
            if state.snapshot.enabled {
                // As Mutter (48) does: no barriers while the session is
                // enabled; every one comes back refused, the old ones stay.
                state.snapshot.refused_sets += 1;
                parsed.iter().map(|b| b.0).collect()
            } else {
                state.snapshot.barriers = parsed;
                state.snapshot.barrier_sets += 1;
                Vec::new()
            }
        };
        respond(connection, request.clone(), BarriersResults { failed_barriers });
        request
    }

    async fn enable(&self, _session: OwnedObjectPath, _options: HashMap<String, OwnedValue>) {
        let mut state = self.0.lock().unwrap();
        state.snapshot.enabled = true;
        state.snapshot.enables += 1;
    }

    async fn disable(&self, _session: OwnedObjectPath, _options: HashMap<String, OwnedValue>) {
        let mut state = self.0.lock().unwrap();
        state.snapshot.enabled = false;
        state.snapshot.disables += 1;
    }

    async fn release(
        &self,
        #[zbus(connection)] connection: &Connection,
        session: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
    ) {
        let activation_id = options.get("activation_id").and_then(|v| u32::try_from(v).ok());
        let cursor = options
            .get("cursor_position")
            .and_then(|v| <(f64, f64)>::try_from(v.clone()).ok());
        log::info!("InputCapture.Release activation={activation_id:?} cursor={cursor:?}");
        self.0.lock().unwrap().snapshot.releases.push((activation_id, cursor));
        let connection = connection.clone();
        tokio::spawn(async move {
            let _ = connection
                .emit_signal(
                    None::<zbus::names::BusName<'_>>,
                    DESKTOP_PATH,
                    "org.freedesktop.portal.InputCapture",
                    "Deactivated",
                    &(session, DeactivatedOptions { activation_id: activation_id.unwrap_or(0) }),
                )
                .await;
        });
    }

    #[zbus(name = "ConnectToEIS")]
    async fn connect_to_eis(
        &self,
        _session: OwnedObjectPath,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<zbus::zvariant::OwnedFd> {
        let (ours, theirs) = UnixStream::pair().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let state = self.0.lock().unwrap();
        let _ = state.new_eis.send((Kind::Capture, ours));
        Ok(zbus::zvariant::OwnedFd::from(std::os::fd::OwnedFd::from(theirs)))
    }
}

struct RemoteDesktopIface(Shared);

#[interface(name = "org.freedesktop.portal.RemoteDesktop")]
impl RemoteDesktopIface {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }

    #[zbus(property)]
    fn available_device_types(&self) -> u32 {
        1 | 2
    }

    async fn create_session(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        options: HashMap<String, OwnedValue>,
    ) -> OwnedObjectPath {
        let request = request_path(&header, &options, "handle_token");
        let session = request_path(&header, &options, "session_handle_token");
        register_session(connection, &session).await;
        self.0.lock().unwrap().snapshot.emulation_session = Some(session.to_string());
        log::info!("RemoteDesktop.CreateSession → {session}");
        respond(connection, request.clone(), SessionOnlyResults { session_handle: session });
        request
    }

    async fn select_devices(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        _session: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
    ) -> OwnedObjectPath {
        let request = request_path(&header, &options, "handle_token");
        if let Some(token) = options.get("restore_token").and_then(|v| <&str>::try_from(v).ok()) {
            self.0.lock().unwrap().snapshot.restore_token_seen = Some(token.to_owned());
        }
        respond(connection, request.clone(), Empty {});
        request
    }

    async fn start(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        _session: OwnedObjectPath,
        _parent_window: &str,
        options: HashMap<String, OwnedValue>,
    ) -> OwnedObjectPath {
        let request = request_path(&header, &options, "handle_token");
        let clipboard_enabled = self.0.lock().unwrap().snapshot.clipboard_requested;
        respond(connection, request.clone(), StartResults { devices: 3, restore_token: RESTORE_TOKEN.into(), clipboard_enabled });
        request
    }

    #[zbus(name = "ConnectToEIS")]
    async fn connect_to_eis(
        &self,
        _session: OwnedObjectPath,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<zbus::zvariant::OwnedFd> {
        let (ours, theirs) = UnixStream::pair().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let state = self.0.lock().unwrap();
        let _ = state.new_eis.send((Kind::Emulation, ours));
        Ok(zbus::zvariant::OwnedFd::from(std::os::fd::OwnedFd::from(theirs)))
    }
}

struct ClipboardIface(Shared);

fn pipe_fd<F: Into<std::os::fd::OwnedFd>>(end: F) -> zbus::zvariant::OwnedFd {
    zbus::zvariant::OwnedFd::from(end.into())
}

#[interface(name = "org.freedesktop.portal.Clipboard")]
impl ClipboardIface {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }

    async fn request_clipboard(&self, _session: OwnedObjectPath, _options: HashMap<String, OwnedValue>) {
        log::info!("Clipboard.RequestClipboard");
        self.0.lock().unwrap().snapshot.clipboard_requested = true;
    }

    async fn set_selection(
        &self,
        #[zbus(connection)] connection: &Connection,
        session: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
    ) {
        let types: Vec<String> = options
            .get("mime_types")
            .and_then(|v| <Vec<String>>::try_from(v.clone()).ok())
            .unwrap_or_default();
        log::info!("Clipboard.SetSelection {types:?}");
        {
            let mut state = self.0.lock().unwrap();
            state.snapshot.offered_types = types.clone();
            state.snapshot.selection_sets += 1;
            state.desktop_clipboard.clear();
        }
        // As Mutter does: the new owner is announced to the session too.
        let _ = connection
            .emit_signal(
                None::<zbus::names::BusName<'_>>,
                DESKTOP_PATH,
                "org.freedesktop.portal.Clipboard",
                "SelectionOwnerChanged",
                &(session, OwnerChanged { mime_types: types, session_is_owner: true }),
            )
            .await;
    }

    async fn selection_read(&self, _session: OwnedObjectPath, mime_type: String) -> zbus::fdo::Result<zbus::zvariant::OwnedFd> {
        let data = {
            let mut state = self.0.lock().unwrap();
            state.snapshot.reads.push(mime_type.clone());
            state.desktop_clipboard.iter().find(|(m, _)| *m == mime_type).map(|(_, d)| d.clone())
        };
        let Some(data) = data else {
            return Err(zbus::fdo::Error::Failed(format!("{mime_type} not offered")));
        };
        let (reader, mut writer) = std::io::pipe().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = writer.write_all(&data);
        });
        Ok(pipe_fd(reader))
    }

    async fn selection_write(&self, _session: OwnedObjectPath, serial: u32) -> zbus::fdo::Result<zbus::zvariant::OwnedFd> {
        let (mut reader, writer) = std::io::pipe().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let shared = self.0.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut data = Vec::new();
            let _ = reader.read_to_end(&mut data);
            if let Some(paste) = shared.lock().unwrap().snapshot.pastes.get_mut(&serial) {
                paste.data = hex(&data);
            }
        });
        Ok(pipe_fd(writer))
    }

    async fn selection_write_done(&self, _session: OwnedObjectPath, serial: u32, success: bool) {
        log::info!("Clipboard.SelectionWriteDone {serial} {success}");
        if let Some(paste) = self.0.lock().unwrap().snapshot.pastes.get_mut(&serial) {
            paste.done = Some(success);
        }
    }
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .filter_map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

/// One EIS connection: the handshake, a seat, devices on `Bind`, then
/// either sending what the orchestrator injects (capture side) or recording
/// what the agent plays (emulation side).
async fn eis_server(stream: UnixStream, kind: Kind, shared: Shared, mut inject: Option<mpsc::UnboundedReceiver<Inject>>) {
    stream.set_nonblocking(true).expect("nonblocking");
    let context = eis::Context::new(stream).expect("eis context");
    #[allow(deprecated)]
    let fd = AsyncFd::new(context).expect("async fd");
    let mut handshaker = Some(EisHandshaker::new(fd.get_ref(), 1));
    let mut converter: Option<EisRequestConverter> = None;
    let mut pointer: Option<reis::request::Device> = None;
    let mut keyboard: Option<reis::request::Device> = None;
    // Kept: dropping the handle would remove the seat.
    let mut seat: Option<reis::request::Seat> = None;
    let mut sequence = 0u32;
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_micros() as u64)
    };
    let record = |shared: &Shared, value: serde_json::Value| {
        shared.lock().unwrap().snapshot.received.push(value);
    };
    'outer: loop {
        tokio::select! {
            guard = fd.readable() => {
                let mut guard = match guard { Ok(guard) => guard, Err(_) => break };
                // `read` drains the socket until it would block (0 means
                // nothing new); only `UnexpectedEof` is the end.
                let outcome = guard.get_inner().read();
                guard.clear_ready();
                match outcome {
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(error) => { log::info!("eis {kind:?} read: {error}"); break }
                }
                while let Some(pending) = fd.get_ref().pending_request() {
                    let request = match pending {
                        reis::PendingRequestResult::Request(request) => request,
                        reis::PendingRequestResult::ParseError(error) => { log::warn!("eis parse: {error:?}"); break 'outer }
                        reis::PendingRequestResult::InvalidObject(id) => { log::warn!("eis invalid object {id}"); continue }
                    };
                    if let Some(h) = handshaker.as_mut() {
                        match h.handle_request(request) {
                            Ok(Some(response)) => {
                                let c = EisRequestConverter::new(fd.get_ref(), response, 1);
                                let connection = c.handle().clone();
                                seat = Some(connection.add_seat(Some("default"),
                                    DeviceCapability::Pointer | DeviceCapability::PointerAbsolute
                                    | DeviceCapability::Keyboard | DeviceCapability::Button | DeviceCapability::Scroll));
                                let _ = connection.flush();
                                converter = Some(c);
                                handshaker = None;
                                let mut state = shared.lock().unwrap();
                                match kind { Kind::Capture => state.snapshot.capture_eis = true, Kind::Emulation => state.snapshot.emulation_eis = true }
                                log::info!("eis {kind:?}: handshake done");
                            }
                            Ok(None) => {}
                            Err(error) => { log::warn!("eis handshake: {error:?}"); break 'outer }
                        }
                        continue;
                    }
                    let Some(c) = converter.as_mut() else { continue };
                    if let Err(error) = c.handle_request(request) { log::warn!("eis request: {error:?}"); break 'outer }
                    while let Some(request) = c.next_request() {
                        match request {
                            EisRequest::Disconnect => break 'outer,
                            EisRequest::Bind(bind) => {
                                let caps = bind.capabilities;
                                let receiver = c.handle().context_type() == eis::handshake::ContextType::Receiver;
                                let (w, h, x, y) = shared.lock().unwrap().snapshot.zones.first().copied().unwrap_or((1920, 1080, 0, 0));
                                let finish = |device: reis::request::Device, sequence: &mut u32| {
                                    device.resumed();
                                    if receiver { *sequence += 1; device.start_emulating(*sequence); }
                                    device
                                };
                                if caps.contains(DeviceCapability::Pointer) && pointer.is_none() {
                                    let device = bind.seat.add_device(Some("pointer"), eis::device::DeviceType::Virtual,
                                        DeviceCapability::Pointer | DeviceCapability::Button | DeviceCapability::Scroll, |_| {});
                                    pointer = Some(finish(device, &mut sequence));
                                }
                                if caps.contains(DeviceCapability::PointerAbsolute) {
                                    // With a region, as a compositor that maps absolute
                                    // coordinates to a screen does.
                                    let device = bind.seat.add_device(Some("pointer-abs"), eis::device::DeviceType::Virtual,
                                        DeviceCapability::PointerAbsolute | DeviceCapability::Button | DeviceCapability::Scroll,
                                        |d| d.device().region(x.max(0) as u32, y.max(0) as u32, w, h, 1.0));
                                    finish(device, &mut sequence);
                                }
                                if caps.contains(DeviceCapability::Keyboard) && keyboard.is_none() {
                                    let device = bind.seat.add_device(Some("keyboard"), eis::device::DeviceType::Virtual, DeviceCapability::Keyboard.into(), |_| {});
                                    keyboard = Some(finish(device, &mut sequence));
                                }
                                log::info!("eis {kind:?}: bound {caps:?}");
                            }
                            EisRequest::DeviceStartEmulating(e) => record(&shared, serde_json::json!({"type": "start_emulating", "device": format!("{:?}", e.device.name()), "sequence": e.sequence})),
                            EisRequest::PointerMotion(m) => record(&shared, serde_json::json!({"type": "rel", "dx": m.dx, "dy": m.dy})),
                            EisRequest::PointerMotionAbsolute(m) => record(&shared, serde_json::json!({"type": "abs", "x": m.dx_absolute, "y": m.dy_absolute})),
                            EisRequest::Button(b) => record(&shared, serde_json::json!({"type": "button", "code": b.button, "down": b.state == eis::button::ButtonState::Press})),
                            EisRequest::KeyboardKey(k) => record(&shared, serde_json::json!({"type": "key", "code": k.key, "down": k.state == eis::keyboard::KeyState::Press})),
                            EisRequest::ScrollDelta(s) => record(&shared, serde_json::json!({"type": "scroll", "dx": s.dx, "dy": s.dy})),
                            EisRequest::ScrollDiscrete(s) => record(&shared, serde_json::json!({"type": "scroll_discrete", "dx": s.discrete_dx, "dy": s.discrete_dy})),
                            _ => {}
                        }
                    }
                    let _ = c.handle().flush();
                }
            }
            event = async { match inject.as_mut() { Some(rx) => rx.recv().await, None => std::future::pending().await } } => {
                let Some(event) = event else { break };
                let Some(c) = converter.as_ref() else { continue };
                let time = now();
                match event {
                    Inject::Motion(dx, dy) => if let Some(d) = &pointer && let Some(p) = d.interface::<eis::Pointer>() { p.motion_relative(dx as f32, dy as f32); d.frame(time); },
                    Inject::Button(code, down) => if let Some(d) = &pointer && let Some(b) = d.interface::<eis::Button>() { b.button(code, if down { eis::button::ButtonState::Press } else { eis::button::ButtonState::Released }); d.frame(time); },
                    Inject::Scroll(dx, dy) => if let Some(d) = &pointer && let Some(s) = d.interface::<eis::Scroll>() { s.scroll(dx as f32, dy as f32); d.frame(time); },
                    Inject::Key(code, down) => if let Some(d) = &keyboard && let Some(k) = d.interface::<eis::Keyboard>() { k.key(code, if down { eis::keyboard::KeyState::Press } else { eis::keyboard::KeyState::Released }); d.frame(time); },
                }
                let _ = c.handle().flush();
            }
        }
    }
    let mut state = shared.lock().unwrap();
    match kind {
        Kind::Capture => { state.snapshot.capture_eis = false; state.receiver = None; }
        Kind::Emulation => state.snapshot.emulation_eis = false,
    }
    drop(seat);
    log::info!("eis {kind:?}: connection over");
}

async fn control(listener: tokio::net::UnixListener, shared: Shared, connection: Connection) {
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let shared = shared.clone();
        let connection = connection.clone();
        tokio::task::spawn_local(async move {
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let reply = handle_command(&line, &shared, &connection).await;
                let mut text = reply.to_string();
                text.push('\n');
                if writer.write_all(text.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
    }
}

async fn handle_command(line: &str, shared: &Shared, connection: &Connection) -> serde_json::Value {
    let Ok(command) = serde_json::from_str::<serde_json::Value>(line) else {
        return serde_json::json!({"ok": false, "error": "bad json"});
    };
    let number = |key: &str| command[key].as_f64().unwrap_or(0.0);
    match command["cmd"].as_str().unwrap_or("") {
        "state" => {
            let state = shared.lock().unwrap();
            serde_json::json!({"ok": true, "state": serde_json::to_value(&state.snapshot).unwrap()})
        }
        "activate" => {
            let (session, id) = {
                let mut state = shared.lock().unwrap();
                state.activation += 1;
                (state.snapshot.capture_session.clone(), state.activation)
            };
            let Some(session) = session else {
                return serde_json::json!({"ok": false, "error": "no capture session"});
            };
            let options = ActivatedOptions {
                activation_id: id,
                cursor_position: (number("x"), number("y")),
                barrier_id: command["barrier"].as_u64().unwrap_or(1) as u32,
            };
            let path = OwnedObjectPath::try_from(session).unwrap();
            let result = connection
                .emit_signal(None::<zbus::names::BusName<'_>>, DESKTOP_PATH, "org.freedesktop.portal.InputCapture", "Activated", &(path, options))
                .await;
            serde_json::json!({"ok": result.is_ok(), "activation_id": id})
        }
        "motion" | "key" | "button" | "scroll" => {
            let event = match command["cmd"].as_str().unwrap() {
                "motion" => Inject::Motion(number("dx"), number("dy")),
                "scroll" => Inject::Scroll(number("dx"), number("dy")),
                "key" => Inject::Key(command["code"].as_u64().unwrap_or(0) as u32, command["down"].as_bool().unwrap_or(true)),
                _ => Inject::Button(command["code"].as_u64().unwrap_or(0x110) as u32, command["down"].as_bool().unwrap_or(true)),
            };
            let sent = shared.lock().unwrap().receiver.as_ref().is_some_and(|tx| tx.send(event).is_ok());
            serde_json::json!({"ok": sent})
        }
        "copy" => {
            let formats: Vec<(String, Vec<u8>)> = command["formats"]
                .as_array()
                .map(|formats| {
                    formats
                        .iter()
                        .filter_map(|f| Some((f[0].as_str()?.to_owned(), unhex(f[1].as_str()?))))
                        .collect()
                })
                .unwrap_or_default();
            let (session, types) = {
                let mut state = shared.lock().unwrap();
                state.desktop_clipboard = formats.clone();
                (state.snapshot.emulation_session.clone(), formats.iter().map(|(m, _)| m.clone()).collect::<Vec<_>>())
            };
            let Some(session) = session else {
                return serde_json::json!({"ok": false, "error": "no remote desktop session"});
            };
            let path = OwnedObjectPath::try_from(session).unwrap();
            let result = connection
                .emit_signal(None::<zbus::names::BusName<'_>>, DESKTOP_PATH, "org.freedesktop.portal.Clipboard", "SelectionOwnerChanged",
                    &(path, OwnerChangedAsSent { mime_types: (types,), session_is_owner: false }))
                .await;
            serde_json::json!({"ok": result.is_ok()})
        }
        "systemd_env" => {
            let env: Vec<String> = command["env"]
                .as_array()
                .map(|values| values.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            log::info!("systemd environment: {env:?}");
            shared.lock().unwrap().systemd_env = env;
            serde_json::json!({"ok": true})
        }
        "paste" => {
            let mime = command["mime"].as_str().unwrap_or("text/plain;charset=utf-8").to_owned();
            let (session, serial) = {
                let mut state = shared.lock().unwrap();
                state.transfer_serial += 1;
                let serial = state.transfer_serial;
                state.snapshot.pastes.insert(serial, Paste { mime: mime.clone(), ..Paste::default() });
                (state.snapshot.emulation_session.clone(), serial)
            };
            let Some(session) = session else {
                return serde_json::json!({"ok": false, "error": "no remote desktop session"});
            };
            let path = OwnedObjectPath::try_from(session).unwrap();
            let result = connection
                .emit_signal(None::<zbus::names::BusName<'_>>, DESKTOP_PATH, "org.freedesktop.portal.Clipboard", "SelectionTransfer", &(path, mime, serial))
                .await;
            serde_json::json!({"ok": result.is_ok(), "serial": serial})
        }
        other => serde_json::json!({"ok": false, "error": format!("unknown command {other}")}),
    }
}

fn parse_zones(spec: &str) -> Vec<(u32, u32, i32, i32)> {
    spec.split(',')
        .filter_map(|zone| {
            let (size, offset) = zone.split_once('+').map_or((zone, "0+0"), |(s, o)| (s, o));
            let (w, h) = size.split_once('x')?;
            let (x, y) = offset.split_once('+').unwrap_or((offset, "0"));
            Some((w.parse().ok()?, h.parse().ok()?, x.parse().ok()?, y.parse().ok()?))
        })
        .collect()
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let mut control_path = None;
    let mut zones = vec![(1920, 1080, 0, 0)];
    let mut systemd = false;
    let mut iter = arguments.iter();
    while let Some(argument) = iter.next() {
        match argument.as_str() {
            "--control" => control_path = iter.next().cloned(),
            "--zones" => zones = parse_zones(iter.next().map(String::as_str).unwrap_or("")),
            "--systemd" => systemd = true,
            other => {
                eprintln!("fakedesk: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let control_path = control_path.expect("--control <socket path>");
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async move {
        let (new_eis, mut eis_rx) = mpsc::unbounded_channel::<(Kind, UnixStream)>();
        let shared: Shared = Arc::new(Mutex::new(State {
            snapshot: Snapshot { zones, ..Snapshot::default() },
            activation: 0,
            zone_set: 1,
            receiver: None,
            new_eis,
            desktop_clipboard: Vec::new(),
            transfer_serial: 0,
            systemd_env: Vec::new(),
        }));
        let connection = zbus::connection::Builder::session()
            .expect("session bus")
            .name("org.freedesktop.portal.Desktop")
            .expect("name")
            .serve_at(DESKTOP_PATH, InputCaptureIface(shared.clone()))
            .expect("serve")
            .serve_at(DESKTOP_PATH, RemoteDesktopIface(shared.clone()))
            .expect("serve")
            .serve_at(DESKTOP_PATH, ClipboardIface(shared.clone()))
            .expect("serve")
            .build()
            .await
            .expect("connect to the session bus");
        log::info!("serving org.freedesktop.portal.Desktop as {:?}", connection.unique_name().map(|n| n.to_string()));
        if systemd {
            connection.object_server().at("/org/freedesktop/systemd1", SystemdManager(shared.clone())).await.expect("serve systemd");
            connection.request_name("org.freedesktop.systemd1").await.expect("systemd name");
        }
        let _ = std::fs::remove_file(&control_path);
        let listener = tokio::net::UnixListener::bind(&control_path).expect("control socket");
        tokio::task::spawn_local(control(listener, shared.clone(), connection.clone()));
        while let Some((kind, stream)) = eis_rx.recv().await {
            let inject = match kind {
                Kind::Capture => {
                    let (tx, rx) = mpsc::unbounded_channel();
                    shared.lock().unwrap().receiver = Some(tx);
                    Some(rx)
                }
                Kind::Emulation => None,
            };
            tokio::task::spawn_local(eis_server(stream, kind, shared.clone(), inject));
        }
    });
}
