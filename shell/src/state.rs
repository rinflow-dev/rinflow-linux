//! The agent's view of the world, folded from core events, which the
//! window renders as it is. The agent pushes the whole snapshot on every
//! change, so the window never has to replay individual core events.

use crate::i18n::{Text, msg};
use crate::settings::Settings;
use rinflow_core::layout::{Display, Offset, Side};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::watch;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Peer {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub platform: String,
    pub address: String,
    pub port: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreStatus {
    Starting,
    Running,
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Connecting,
    Connected,
}

/// One peer this device is connecting or connected to. The core keeps one
/// session per peer and several at once.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub peer_id: String,
    /// The peer's name, or its ID until it is known.
    pub name: String,
    pub status: SessionStatus,
    /// This peer is currently driving this device's mouse and keyboard.
    pub remote_control: bool,
    /// Round trip to the peer, in whole milliseconds, once measured.
    pub latency_ms: Option<u32>,
    /// The peer's certificate is the one paired with.
    pub paired: bool,
}

/// Pairing in progress: the code both devices show.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PairingView {
    pub peer_id: String,
    pub name: String,
    pub platform: String,
    pub code: String,
    /// This side's user accepted; waiting for the other device.
    pub accepted: bool,
}

/// The arrangement page: this device's displays and every known peer, all
/// in logical points; placed peers carry their offset in this device's space.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LayoutView {
    pub displays: Vec<Display>,
    pub devices: Vec<DeviceView>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceView {
    pub peer_id: String,
    pub name: String,
    pub platform: String,
    /// The peer's displays in its own space (a default size until known).
    pub displays: Vec<Display>,
    pub offset: Option<Offset>,
    /// The side of this device the peer is attached to, if it touches one.
    pub side: Option<Side>,
    /// One of the user's devices: connected at least once, or placed.
    /// Others are only nearby.
    pub remembered: bool,
    /// When the peer was last seen online (Unix milliseconds), if ever.
    pub last_seen_ms: Option<u64>,
}

impl TransferView {
    /// How much has arrived, from 0 to 100.
    pub fn percent(&self) -> u64 {
        (self.received.min(self.total) * 100).checked_div(self.total).unwrap_or(100)
    }
}

/// How one direction of input stands on this desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendState {
    /// Nothing found yet, or the desktop has not answered: trying.
    Waiting,
    /// The desktop asked the user and is waiting for the answer.
    Prompting,
    Ready,
    /// The user declined; `retry_input` asks again.
    Denied,
    /// This desktop offers no way to do it (for example capture under
    /// gamescope). Nothing to retry.
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendView {
    pub state: BackendState,
    /// `portal`, `gamescope`, `x11`, or empty while nothing is chosen.
    pub backend: String,
    /// One line for the user when it is not `ready` (empty: nothing to say).
    #[serde(default)]
    pub detail: Text,
}

impl BackendView {
    pub fn new(state: BackendState, backend: &str, detail: impl Into<Text>) -> Self {
        Self { state, backend: backend.to_owned(), detail: detail.into() }
    }
}

impl Default for BackendView {
    fn default() -> Self {
        Self::new(BackendState::Waiting, "", Text::default())
    }
}

/// Capture (driving peers) and emulation (being driven).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputView {
    pub capture: BackendView,
    pub emulation: BackendView,
    /// The desktop gave the emulation session its clipboard, so it can be
    /// shared (with the `clipboard` setting on).
    #[serde(default)]
    pub clipboard: bool,
}

/// Everything the window renders.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub device_id: String,
    pub device_name: String,
    pub core: CoreStatus,
    pub peers: Vec<Peer>,
    pub sessions: Vec<Session>,
    pub layout: LayoutView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<Text>,
    /// Preferences (see `settings.rs`).
    pub settings: Settings,
    /// The agent starts when the user signs in.
    pub launch_at_login: bool,
    /// The port the core listens on now; `settings.port` applies after a
    /// restart.
    pub control_port: u16,
    pub agent_version: String,
    /// SHA-256 of this device's certificate (hex), once the core runs.
    pub fingerprint: Option<String>,
    /// A pairing waiting for the user.
    pub pairing: Option<PairingView>,
    /// Device IDs of paired peers.
    pub paired_peers: Vec<String>,
    /// The user quit the agent: the window quits too instead of starting
    /// the agent again.
    pub quitting: bool,
    /// The addresses the user added and how each fares: the core's
    /// `manual_addresses` entries (`address`, `status`, `peer_id`, `name`,
    /// `error`).
    pub manual_addresses: Vec<Value>,
    /// Which peer this device drives right now, if any.
    pub driving: Option<String>,
    /// The pointer is kept on the screen it is on: no edge crosses. Not
    /// kept between runs.
    #[serde(default)]
    pub locked: bool,
    /// Files a peer copied, being received now.
    #[serde(default)]
    pub transfer: Option<TransferView>,
    /// How input stands on this desktop.
    pub input: InputView,
}

/// Files copied on another device being pasted here (read from it).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransferView {
    /// The core's number for it (`cancel_files`).
    pub transfer: u32,
    pub peer_id: String,
    /// The first file or folder's name, and how many there are at the top.
    pub name: String,
    pub items: u64,
    pub total: u64,
    pub received: u64,
}

#[derive(Clone)]
pub struct SharedState(Arc<watch::Sender<Snapshot>>);

impl SharedState {
    pub fn new(
        device_id: &str,
        device_name: &str,
        settings: Settings,
        launch_at_login: bool,
        control_port: u16,
        agent_version: &str,
    ) -> Self {
        Self(Arc::new(watch::Sender::new(Snapshot {
            device_id: device_id.to_owned(),
            device_name: device_name.to_owned(),
            core: CoreStatus::Starting,
            peers: Vec::new(),
            sessions: Vec::new(),
            layout: LayoutView::default(),
            last_error: None,
            settings,
            launch_at_login,
            control_port,
            agent_version: agent_version.to_owned(),
            fingerprint: None,
            pairing: None,
            paired_peers: Vec::new(),
            quitting: false,
            manual_addresses: Vec::new(),
            driving: None,
            locked: false,
            transfer: None,
            input: InputView::default(),
        })))
    }

    pub fn snapshot(&self) -> Snapshot {
        self.0.borrow().clone()
    }

    pub fn set_quitting(&self) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.quitting, true));
    }

    pub fn set_paired_peers(&self, peers: Vec<String>) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.paired_peers, peers));
    }

    /// This side's user accepted the pairing with `peer_id`.
    pub fn set_pairing_accepted(&self, peer_id: &str) {
        self.0
            .send_if_modified(|snapshot| match &mut snapshot.pairing {
                Some(pairing) if pairing.peer_id == peer_id => replace(&mut pairing.accepted, true),
                _ => false,
            });
    }

    pub fn set_settings(&self, settings: Settings) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.settings, settings));
    }

    pub fn set_launch_at_login(&self, enabled: bool) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.launch_at_login, enabled));
    }

    pub fn set_driving(&self, peer_id: Option<String>) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.driving, peer_id));
    }

    /// Locks the pointer to its screen or frees it (`None`: the other of
    /// the two); the new state.
    pub fn set_locked(&self, locked: Option<bool>) -> bool {
        let mut now = false;
        self.0.send_if_modified(|snapshot| {
            now = locked.unwrap_or(!snapshot.locked);
            replace(&mut snapshot.locked, now)
        });
        now
    }

    /// How much of the files being pasted from `peer_id` has been read, as
    /// the agent counts it. The core counts only what arrived in order,
    /// and a file manager reads ahead out of order.
    pub fn set_files_read(&self, peer_id: &str, received: u64) {
        self.0.send_if_modified(|snapshot| match &mut snapshot.transfer {
            Some(view) if view.peer_id == peer_id => advance(view, received),
            _ => false,
        });
    }

    pub fn locked(&self) -> bool {
        self.0.borrow().locked
    }

    pub fn set_capture(&self, view: BackendView) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.input.capture, view));
    }

    pub fn set_emulation(&self, view: BackendView) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.input.emulation, view));
    }

    pub fn set_clipboard_available(&self, available: bool) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.input.clipboard, available));
    }

    pub fn clear_error(&self) {
        self.0
            .send_if_modified(|snapshot| snapshot.last_error.take().is_some());
    }

    pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.0.subscribe()
    }

    /// Whether a session with the peer is established.
    pub fn is_connected(&self, peer_id: &str) -> bool {
        self.0
            .borrow()
            .sessions
            .iter()
            .any(|session| session.peer_id == peer_id && session.status == SessionStatus::Connected)
    }

    pub fn set_layout(&self, layout: LayoutView) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.layout, layout));
    }

    pub fn set_error(&self, message: impl Into<Text>) {
        let message = message.into();
        self.0
            .send_modify(|snapshot| snapshot.last_error = Some(message));
    }

    /// Folds one JSON event from `rinflow_core::Core` into the snapshot.
    /// Runs on the core's threads.
    pub fn apply_core_event(&self, json: &str) {
        let Ok(event) = serde_json::from_str::<Value>(json) else {
            return;
        };
        let Some(kind) = event["event"].as_str() else {
            return;
        };
        self.0.send_if_modified(|snapshot| match kind {
            "started" => {
                let fingerprint = event["fingerprint"].as_str().map(str::to_owned);
                replace(&mut snapshot.core, CoreStatus::Running)
                    | replace(&mut snapshot.fingerprint, fingerprint)
            }
            "pairing_requested" => {
                let Some(peer_id) = event["peer_id"].as_str() else {
                    return false;
                };
                // One pairing at a time: another device asking while one is
                // pending must not take the place of the one the user is
                // checking (it could carry the same name). Its request times
                // out in the core; it can ask again once this one is over.
                if snapshot.pairing.as_ref().is_some_and(|pending| pending.peer_id != peer_id) {
                    log::info!("pairing request from {peer_id} ignored: another one is pending");
                    return false;
                }
                let text = |key: &str| event[key].as_str().unwrap_or_default().to_owned();
                snapshot.pairing = Some(PairingView {
                    peer_id: peer_id.to_owned(),
                    name: event["name"].as_str().unwrap_or(peer_id).to_owned(),
                    platform: text("platform"),
                    code: text("code"),
                    accepted: false,
                });
                true
            }
            "paired" => {
                let Some(peer_id) = event["peer_id"].as_str() else {
                    return false;
                };
                if !snapshot.paired_peers.iter().any(|id| id == peer_id) {
                    snapshot.paired_peers.push(peer_id.to_owned());
                }
                if snapshot
                    .pairing
                    .as_ref()
                    .is_some_and(|p| p.peer_id == peer_id)
                {
                    snapshot.pairing = None;
                }
                if let Some(session) = snapshot.sessions.iter_mut().find(|s| s.peer_id == peer_id) {
                    session.paired = true;
                }
                true
            }
            "unpaired" => {
                let Some(peer_id) = event["peer_id"].as_str() else {
                    return false;
                };
                let before = snapshot.paired_peers.len();
                snapshot.paired_peers.retain(|id| id != peer_id);
                snapshot.paired_peers.len() != before
            }
            "pairing_ended" => {
                let peer_id = event["peer_id"].as_str();
                if snapshot
                    .pairing
                    .as_ref()
                    .is_some_and(|p| Some(p.peer_id.as_str()) == peer_id)
                {
                    snapshot.pairing = None;
                    return true;
                }
                false
            }
            "stopped" => replace(&mut snapshot.core, CoreStatus::Stopped),
            "peer_up" => {
                let Some(peer) = parse_peer(&event["peer"]) else {
                    return false;
                };
                if snapshot.peers.iter().any(|known| known.id == peer.id) {
                    return false;
                }
                snapshot.peers.push(peer);
                snapshot.peers.sort_by_key(|peer| peer.name.to_lowercase());
                true
            }
            "peer_down" => {
                let Some(peer_id) = event["peer_id"].as_str() else {
                    return false;
                };
                let before = snapshot.peers.len();
                snapshot.peers.retain(|peer| peer.id != peer_id);
                snapshot.peers.len() != before
            }
            "connecting" => {
                let Some(peer_id) = event["peer_id"].as_str() else {
                    return false;
                };
                let known = snapshot.sessions.iter().any(|s| s.peer_id == peer_id);
                let session = snapshot.session_mut(peer_id);
                replace(&mut session.status, SessionStatus::Connecting) || !known
            }
            "connected" => {
                let Some(peer) = parse_peer(&event["peer"]) else {
                    return false;
                };
                let session = snapshot.session_mut(&peer.id);
                session.name = peer.name;
                session.status = SessionStatus::Connected;
                session.paired = event["paired"].as_bool().unwrap_or(false);
                snapshot.last_error = None;
                true
            }
            "latency" => {
                let (Some(peer_id), Some(ms)) = (event["peer_id"].as_str(), event["ms"].as_f64())
                else {
                    return false;
                };
                let ms = ms.round().clamp(0.0, f64::from(u32::MAX)) as u32;
                match snapshot.sessions.iter_mut().find(|s| s.peer_id == peer_id) {
                    Some(session) => replace(&mut session.latency_ms, Some(ms)),
                    None => false,
                }
            }
            "remote_control_started" | "remote_control_ended" => {
                let Some(peer_id) = event["peer_id"].as_str() else {
                    return false;
                };
                let active = kind == "remote_control_started";
                match snapshot.sessions.iter_mut().find(|s| s.peer_id == peer_id) {
                    Some(session) => replace(&mut session.remote_control, active),
                    None => false,
                }
            }
            "disconnected" => {
                // Sessions are per peer; a disconnect without a peer ID does
                // not concern any session shown here.
                let Some(peer_id) = event["peer_id"].as_str() else {
                    return false;
                };
                let name = snapshot
                    .sessions
                    .iter()
                    .find(|session| session.peer_id == peer_id)
                    .map(|session| session.name.clone())
                    .unwrap_or_else(|| peer_id.to_owned());
                snapshot
                    .sessions
                    .retain(|session| session.peer_id != peer_id);
                if snapshot
                    .pairing
                    .as_ref()
                    .is_some_and(|p| p.peer_id == peer_id)
                {
                    snapshot.pairing = None;
                }
                if snapshot.driving.as_deref() == Some(peer_id) {
                    snapshot.driving = None;
                }
                let reason = event["reason"].as_str().unwrap_or("disconnected");
                if let Some(message) = disconnect_message(reason) {
                    // Translators: {name} is a device's name, {message} why it disconnected.
                    snapshot.last_error = Some(msg("{name}: {message}").arg("name", name).arg("message", message));
                }
                true
            }
            "error" => {
                snapshot.last_error = event["message"].as_str().map(Text::from);
                true
            }
            "files_receiving" => {
                let number = |key: &str| event[key].as_u64().unwrap_or(0);
                snapshot.transfer = Some(TransferView {
                    transfer: number("transfer") as u32,
                    peer_id: event["peer_id"].as_str().unwrap_or_default().to_owned(),
                    name: event["name"].as_str().unwrap_or_default().to_owned(),
                    items: number("items").max(1),
                    total: number("total_size"),
                    received: 0,
                });
                true
            }
            "files_progress" => {
                let (Some(transfer), Some(received)) = (event["transfer"].as_u64(), event["received"].as_u64()) else {
                    return false;
                };
                match &mut snapshot.transfer {
                    Some(view) if u64::from(view.transfer) == transfer => advance(view, received),
                    _ => false,
                }
            }
            // The paste is over: what was pasted is where the user put it.
            "files_stream_ended" => {
                let transfer = event["transfer"].as_u64();
                let ours = snapshot.transfer.as_ref().is_some_and(|view| Some(u64::from(view.transfer)) == transfer);
                if ours {
                    snapshot.transfer = None;
                }
                let reason = event["reason"].as_str().unwrap_or_default();
                // Finished, or stopped by the user: not a failure.
                let failed = ours && !matches!(reason, "" | "delivered" | "released" | "cancelled");
                if failed {
                    // Translators: {reason} is the cause, in English.
                    snapshot.last_error = Some(msg("The copied files couldn't be received: {reason}").arg("reason", reason));
                }
                ours
            }
            "manual_addresses" => {
                let addresses = event["addresses"].as_array().cloned().unwrap_or_default();
                replace(&mut snapshot.manual_addresses, addresses)
            }
            _ => false,
        });
    }
}

impl Snapshot {
    /// The session with `peer_id`, added as `connecting` if there is none.
    fn session_mut(&mut self, peer_id: &str) -> &mut Session {
        let index = match self.sessions.iter().position(|s| s.peer_id == peer_id) {
            Some(index) => index,
            None => {
                let name = self
                    .peers
                    .iter()
                    .find(|peer| peer.id == peer_id)
                    .map(|peer| peer.name.clone())
                    .unwrap_or_else(|| peer_id.to_owned());
                self.sessions.push(Session {
                    peer_id: peer_id.to_owned(),
                    name,
                    status: SessionStatus::Connecting,
                    remote_control: false,
                    latency_ms: None,
                    paired: false,
                });
                self.sessions.len() - 1
            }
        };
        &mut self.sessions[index]
    }
}

/// Moves a transfer's progress forward (never back: it is counted in two
/// places); told to the window a hundredth at a time, not chunk by chunk.
fn advance(view: &mut TransferView, received: u64) -> bool {
    let received = received.min(view.total);
    let step = (view.total / 100).max(1);
    if received > view.received && (received / step != view.received / step || received >= view.total) {
        view.received = received;
        true
    } else {
        false
    }
}

fn replace<T: PartialEq>(slot: &mut T, value: T) -> bool {
    if *slot == value {
        return false;
    }
    *slot = value;
    true
}

/// What to tell the user about a session that ended, if anything: ending it
/// here or on the peer, or declining a pairing here, is not an error.
fn disconnect_message(reason: &str) -> Option<Text> {
    Some(match reason {
        "local request"
        | "peer closed the connection"
        | "disconnected on the other device"
        | "going to sleep"
        | "pairing declined" => return None,
        // The peer vanished without closing (slept, lost the network); the
        // core reconnects once it is back.
        "heartbeat timeout" => msg("The other device stopped responding (it may be asleep or offline). It reconnects when it's back."),
        "refused by the other device" => msg("The other device refused the connection. It may have removed this device, or it only allows paired devices. To pair again, select Connect in the device menu."),
        "the device's identity changed" => msg("The device's identity certificate changed, so the connection was refused. If it was reinstalled, remove it in the device menu and pair again."),
        "pairing declined on the other device" => {
            msg("The other device declined pairing.")
        }
        "pairing timed out" => msg("Pairing timed out. It wasn't confirmed within two minutes."),
        "pairing required" => msg("An unpaired device was disconnected (“Only paired devices” is on)."),
        "unpaired" => msg("Unpaired."),
        other => Text::from(other),
    })
}

fn parse_peer(value: &Value) -> Option<Peer> {
    serde_json::from_value(value.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> SharedState {
        SharedState::new("this-box", "This box", Settings::default(), false, 42421, "0.0.0")
    }

    fn apply(state: &SharedState, event: serde_json::Value) {
        state.apply_core_event(&event.to_string());
    }

    #[test]
    fn sessions_follow_connections() {
        let state = state();
        apply(
            &state,
            serde_json::json!({"event": "peer_up", "peer": {"id": "mac", "name": "Mac mini", "address": "10.0.0.2", "port": 42421}}),
        );
        apply(&state, serde_json::json!({"event": "connecting", "peer_id": "mac"}));
        assert_eq!(state.snapshot().sessions[0].name, "Mac mini");
        apply(
            &state,
            serde_json::json!({"event": "connected", "peer": {"id": "mac", "name": "Mac mini", "address": "10.0.0.2", "port": 42421}, "paired": true}),
        );
        assert!(state.is_connected("mac"));
        apply(&state, serde_json::json!({"event": "latency", "peer_id": "mac", "ms": 3.6}));
        assert_eq!(state.snapshot().sessions[0].latency_ms, Some(4));
        state.set_driving(Some("mac".into()));
        apply(
            &state,
            serde_json::json!({"event": "disconnected", "peer_id": "mac", "reason": "local request"}),
        );
        let snapshot = state.snapshot();
        assert!(snapshot.sessions.is_empty());
        assert_eq!(snapshot.driving, None);
        assert_eq!(snapshot.last_error, None);
    }

    #[test]
    fn errors_go_to_the_window_untranslated() {
        let state = state();
        apply(&state, serde_json::json!({"event": "connecting", "peer_id": "mac"}));
        apply(&state, serde_json::json!({"event": "disconnected", "peer_id": "mac", "reason": "unpaired"}));
        assert_eq!(
            serde_json::to_value(state.snapshot().last_error).unwrap(),
            serde_json::json!({"id": "{name}: {message}", "args": {"name": "mac", "message": {"id": "Unpaired."}}})
        );
        apply(&state, serde_json::json!({"event": "error", "message": "control port failed: in use"}));
        assert_eq!(state.snapshot().last_error, Some(Text::from("control port failed: in use")));
    }

    #[test]
    fn pairing_shows_and_clears() {
        let state = state();
        apply(
            &state,
            serde_json::json!({"event": "pairing_requested", "peer_id": "pc", "name": "PC", "platform": "windows", "code": "482917"}),
        );
        assert_eq!(state.snapshot().pairing.as_ref().unwrap().code, "482917");
        state.set_pairing_accepted("pc");
        assert!(state.snapshot().pairing.as_ref().unwrap().accepted);
        apply(&state, serde_json::json!({"event": "paired", "peer_id": "pc", "fingerprint": "ab"}));
        let snapshot = state.snapshot();
        assert!(snapshot.pairing.is_none());
        assert_eq!(snapshot.paired_peers, vec!["pc".to_owned()]);
    }

    #[test]
    fn another_device_cannot_take_the_place_of_a_pending_pairing() {
        let state = state();
        let request = |peer: &str, code: &str| {
            serde_json::json!({"event": "pairing_requested", "peer_id": peer, "name": "PC", "platform": "windows", "code": code})
        };
        apply(&state, request("pc", "482917"));
        apply(&state, request("impostor", "111111"));
        let pairing = state.snapshot().pairing.unwrap();
        assert_eq!((pairing.peer_id.as_str(), pairing.code.as_str()), ("pc", "482917"));
        // The same device with a new code still replaces its own request.
        apply(&state, request("pc", "222333"));
        assert_eq!(state.snapshot().pairing.unwrap().code, "222333");
    }
}
