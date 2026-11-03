//! Paired peers, saved to `trusted.json`.
//!
//! The core decides who is trusted (see rinflow-core's
//! `runtime/trust.rs`); this keeps what it reports across restarts: each
//! `paired` event adds the peer's certificate fingerprint, `unpaired`
//! removes it, and the list goes back to the core as
//! `CoreConfig::trusted_peers`.

use crate::store;
use rinflow_core::TrustedPeer;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct TrustStore {
    path: PathBuf,
    peers: Mutex<Vec<TrustedPeer>>,
}

impl TrustStore {
    pub fn load(directory: &Path) -> Self {
        let path = directory.join("trusted.json");
        let peers: Vec<TrustedPeer> = store::read_json(&path).unwrap_or_default();
        // A hand-edited file must not stop the core from starting.
        let peers = peers
            .into_iter()
            .filter(|peer| {
                peer.fingerprint.len() == 64
                    && peer.fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
            })
            .collect();
        Self {
            path,
            peers: Mutex::new(peers),
        }
    }

    pub fn peers(&self) -> Vec<TrustedPeer> {
        self.peers.lock().map(|peers| peers.clone()).unwrap_or_default()
    }

    pub fn peer_ids(&self) -> Vec<String> {
        self.peers()
            .into_iter()
            .map(|peer| peer.peer_id)
            .collect()
    }

    /// Picks `paired` and `unpaired` out of the core's events; true when
    /// the list changed.
    pub fn core_event(&self, json: &str) -> bool {
        if !json.contains("paired") {
            return false;
        }
        let Ok(event) = serde_json::from_str::<Value>(json) else {
            return false;
        };
        let Some(peer_id) = event["peer_id"].as_str() else {
            return false;
        };
        let Ok(mut peers) = self.peers.lock() else {
            return false;
        };
        match event["event"].as_str() {
            Some("paired") => {
                let Some(fingerprint) = event["fingerprint"].as_str() else {
                    return false;
                };
                peers.retain(|peer| peer.peer_id != peer_id);
                peers.push(TrustedPeer {
                    peer_id: peer_id.to_owned(),
                    fingerprint: fingerprint.to_owned(),
                });
            }
            Some("unpaired") => {
                let before = peers.len();
                peers.retain(|peer| peer.peer_id != peer_id);
                if peers.len() == before {
                    return false;
                }
            }
            _ => return false,
        }
        store::save_json(&self.path, "paired devices", &*peers);
        true
    }
}
