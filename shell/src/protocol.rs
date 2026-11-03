//! What the window (or any other client) and the agent say to each other:
//! JSON, one object per line, over a local socket. The agent greets with
//! `hello`, then pushes the whole [`Snapshot`](crate::state::Snapshot) as
//! `state` on every change; the client sends requests.

use crate::state::Snapshot;
use rinflow_core::layout::{Offset, Side};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Bumped whenever a request or a snapshot field changes meaning.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello {
        protocol: u32,
    },
    Connect {
        peer_id: String,
    },
    Disconnect {
        peer_id: String,
    },
    /// Places a peer near this offset (logical points, this device's space);
    /// the core snaps it to a valid spot and syncs it.
    SetPlacement {
        peer_id: String,
        x: f64,
        y: f64,
    },
    RemovePlacement {
        peer_id: String,
    },
    /// Where `set_placement` with this offset would put the peer, without
    /// placing it: answered with `placement_preview`.
    PreviewPlacement {
        peer_id: String,
        x: f64,
        y: f64,
    },
    PlaceBeside {
        peer_id: String,
        side: Side,
    },
    /// Some keys of `settings` (see `settings.rs`), and `launch_at_login`.
    UpdateSettings {
        values: Map<String, Value>,
    },
    ForgetDevice {
        peer_id: String,
    },
    /// The user compared the pairing code (snapshot `pairing`) and accepts.
    AcceptPairing {
        peer_id: String,
    },
    RejectPairing {
        peer_id: String,
    },
    /// The user dismissed `last_error`.
    ClearError,
    /// Asks the desktop again for permission to capture or play input,
    /// after the user declined or the session was lost.
    RetryInput,
    /// Quits the agent.
    Quit,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message<'a> {
    Hello {
        protocol: u32,
        agent_version: &'static str,
    },
    State(&'a Snapshot),
    Error {
        message: String,
    },
    /// Where a dragged peer would land (`null` when nowhere).
    PlacementPreview {
        peer_id: String,
        offset: Option<Offset>,
    },
}

impl Message<'_> {
    /// One line of the protocol.
    pub fn to_line(&self) -> String {
        let mut line = serde_json::to_string(self).unwrap_or_else(|_| "{}".to_owned());
        line.push('\n');
        line
    }
}

impl Request {
    pub fn parse(line: &str) -> Result<Self, String> {
        serde_json::from_str(line).map_err(|error| format!("invalid request: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        let request = Request::parse(r#"{"type":"set_placement","peer_id":"mac","x":1.5,"y":2}"#)
            .unwrap();
        assert!(matches!(request, Request::SetPlacement { x, .. } if x == 1.5));
        let request = Request::parse(r#"{"type":"place_beside","peer_id":"mac","side":"left"}"#)
            .unwrap();
        assert!(matches!(request, Request::PlaceBeside { side: Side::Left, .. }));
        assert!(Request::parse(r#"{"type":"dance"}"#).is_err());
    }
}
