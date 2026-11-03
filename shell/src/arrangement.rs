//! The arrangement of this device and its peers, as the window shows it.
//!
//! rinflow-core owns placements: it snaps them, syncs them with peers and
//! reports every change as `placement_changed`. This module saves those
//! reports to `layout.json` (handed back to the core at start-up), remembers
//! peers' names and displays so placed devices still show while offline,
//! and derives the view and the portals on this device's edges.

use crate::state::{DeviceView, LayoutView, SharedState};
use crate::store;
use rinflow_core::PlacementRecord;
use rinflow_core::layout::{self, Display, Neighbor, Offset, Portal};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    placements: Vec<PlacementRecord>,
    /// Every peer seen so far, by device ID.
    #[serde(default)]
    devices: BTreeMap<String, KnownDevice>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct KnownDevice {
    name: String,
    #[serde(default)]
    platform: String,
    #[serde(default)]
    displays: Vec<Display>,
    /// Connected at least once: one of the user's devices.
    #[serde(default)]
    remembered: bool,
    /// Last seen online, Unix milliseconds (0: not recorded).
    #[serde(default)]
    last_seen_ms: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

pub struct Arrangement {
    path: PathBuf,
    inner: Mutex<Inner>,
    state: SharedState,
}

/// A placed peer as the pointer crosses into it.
pub struct Target {
    pub peer: Vec<Display>,
    pub offset: Offset,
    /// Its portals with this device (none when it sits against another
    /// peer only).
    pub portals: Vec<Portal>,
    /// The other placed peers, which the pointer may move on into where
    /// they touch this one.
    pub neighbors: Vec<Neighbor>,
}

/// A portal on one of this device's edges and the peer it leads to.
#[derive(Clone, Debug, PartialEq)]
pub struct EdgePortal {
    pub peer_id: String,
    pub portal: Portal,
}

struct Inner {
    saved: Saved,
    displays: Vec<Display>,
    portals: Vec<EdgePortal>,
}

impl Arrangement {
    pub fn load(directory: &Path, state: SharedState, displays: Vec<Display>) -> Arc<Self> {
        let path = directory.join("layout.json");
        let saved = store::read_json::<Saved>(&path).unwrap_or_default();
        let arrangement = Arc::new(Self {
            path,
            inner: Mutex::new(Inner {
                saved,
                displays,
                portals: Vec::new(),
            }),
            state,
        });
        if let Ok(mut inner) = arrangement.inner.lock() {
            arrangement.publish(&mut inner);
        }
        arrangement
    }

    /// Saved placements, for `CoreConfig::placements`.
    pub fn placements(&self) -> Vec<PlacementRecord> {
        self.inner
            .lock()
            .map(|inner| inner.saved.placements.clone())
            .unwrap_or_default()
    }

    pub fn set_displays(&self, displays: Vec<Display>) {
        if let Ok(mut inner) = self.inner.lock() {
            if inner.displays == displays {
                return;
            }
            inner.displays = displays;
            self.publish(&mut inner);
        }
    }

    pub fn displays(&self) -> Vec<Display> {
        self.inner
            .lock()
            .map(|inner| inner.displays.clone())
            .unwrap_or_default()
    }

    /// Portals on this device's edges, in logical points.
    pub fn portals(&self) -> Vec<EdgePortal> {
        self.inner
            .lock()
            .map(|inner| inner.portals.clone())
            .unwrap_or_default()
    }

    /// What controlling a placed peer needs: its displays (in its space),
    /// its offset in this device's space, every portal leading to it, and
    /// the other placed peers it may lead on to.
    pub fn target(&self, peer_id: &str) -> Option<Target> {
        let inner = self.inner.lock().ok()?;
        let offset = inner.offset_of(peer_id)?;
        let peer = inner.displays_of(peer_id);
        let portals = layout::portals(&inner.displays, &peer, offset);
        let neighbors = inner
            .placed()
            .into_iter()
            .filter(|neighbor| neighbor.id != peer_id)
            .collect();
        Some(Target {
            peer,
            offset,
            portals,
            neighbors,
        })
    }

    /// Where a peer dropped at `offset` would land, for showing it while the
    /// user drags: the core's own snapping, with the same displays and the
    /// other placed peers (which it may also sit against). `None` when there
    /// is no room.
    pub fn preview(&self, peer_id: &str, offset: Offset) -> Option<Offset> {
        let inner = self.inner.lock().ok()?;
        let others: Vec<Vec<Display>> = inner
            .placed()
            .into_iter()
            .filter(|other| other.id != peer_id)
            .map(|other| other.displays)
            .collect();
        layout::snap_in_arrangement(
            &inner.displays,
            &inner.displays_of(peer_id),
            offset,
            &others,
        )
    }

    /// Forgets a device: its name, displays and placement. It shows up again
    /// as a nearby device while it is online.
    pub fn forget(&self, peer_id: &str) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.saved.devices.remove(peer_id);
        inner
            .saved
            .placements
            .retain(|record| record.peer_id != peer_id);
        store::save_json(&self.path, "layout", &inner.saved);
        self.publish(&mut inner);
    }

    /// Picks peer descriptions, connections and placement changes out of the
    /// core's events.
    pub fn core_event(&self, json: &str) {
        if !(json.contains("peer_up")
            || json.contains("peer_down")
            || json.contains("peer_displays")
            || json.contains("placement_changed")
            || json.contains("\"connected\"")
            || json.contains("disconnected"))
        {
            return;
        }
        let Ok(event) = serde_json::from_str::<Value>(json) else {
            return;
        };
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        match event["event"].as_str() {
            Some("peer_up") => {
                let peer = &event["peer"];
                let Some(id) = peer["id"].as_str() else {
                    return;
                };
                let device = inner.saved.devices.entry(id.to_owned()).or_default();
                describe(device, id, peer);
                device.last_seen_ms = now_ms();
            }
            Some("connected") => {
                let peer = &event["peer"];
                let Some(id) = peer["id"].as_str() else {
                    return;
                };
                let device = inner.saved.devices.entry(id.to_owned()).or_default();
                describe(device, id, peer);
                device.remembered = true;
                device.last_seen_ms = now_ms();
            }
            Some("peer_down" | "disconnected") => {
                let Some(device) = event["peer_id"]
                    .as_str()
                    .and_then(|id| inner.saved.devices.get_mut(id))
                else {
                    return;
                };
                device.last_seen_ms = now_ms();
            }
            Some("peer_displays") => {
                let Some(id) = event["peer_id"].as_str() else {
                    return;
                };
                let Ok(displays) =
                    serde_json::from_value::<Vec<Display>>(event["displays"].clone())
                else {
                    return;
                };
                let device = inner.saved.devices.entry(id.to_owned()).or_default();
                if device.name.is_empty() {
                    device.name = id.to_owned();
                }
                device.displays = displays;
            }
            Some("placement_changed") => {
                let Some(id) = event["peer_id"].as_str() else {
                    return;
                };
                let offset = serde_json::from_value::<Option<Offset>>(event["offset"].clone())
                    .unwrap_or(None);
                if offset.is_none() && !inner.saved.devices.contains_key(id) {
                    // Removed with a forgotten device: nothing to keep.
                    inner.saved.placements.retain(|record| record.peer_id != id);
                    store::save_json(&self.path, "layout", &inner.saved);
                    self.publish(&mut inner);
                    return;
                }
                let updated_ms = event["updated_ms"].as_u64().unwrap_or_default();
                let record = PlacementRecord {
                    peer_id: id.to_owned(),
                    offset,
                    updated_ms,
                };
                match inner
                    .saved
                    .placements
                    .iter_mut()
                    .find(|existing| existing.peer_id == id)
                {
                    Some(existing) => *existing = record,
                    None => inner.saved.placements.push(record),
                }
            }
            _ => return,
        }
        store::save_json(&self.path, "layout", &inner.saved);
        self.publish(&mut inner);
    }

    fn publish(&self, inner: &mut Inner) {
        let local = inner.displays.clone();
        let mut devices = Vec::new();
        let mut portals = Vec::new();
        let mut ids: Vec<&String> = inner.saved.devices.keys().collect();
        for record in &inner.saved.placements {
            if !inner.saved.devices.contains_key(&record.peer_id) {
                ids.push(&record.peer_id);
            }
        }
        for id in ids {
            let known = inner.saved.devices.get(id).cloned().unwrap_or_default();
            let peer = layout::peer_or_default(&known.displays);
            let offset = inner
                .saved
                .placements
                .iter()
                .find(|record| &record.peer_id == id)
                .and_then(|record| record.offset);
            let touching = offset
                .map(|offset| layout::portals(&local, &peer, offset))
                .unwrap_or_default();
            let remembered = known.remembered || offset.is_some();
            let last_seen_ms = (known.last_seen_ms > 0).then_some(known.last_seen_ms);
            portals.extend(touching.iter().map(|portal| EdgePortal {
                peer_id: id.clone(),
                portal: *portal,
            }));
            devices.push(DeviceView {
                peer_id: id.clone(),
                name: if known.name.is_empty() {
                    id.clone()
                } else {
                    known.name
                },
                platform: known.platform,
                displays: peer,
                offset,
                side: touching.first().map(|portal| portal.side),
                remembered,
                last_seen_ms,
            });
        }
        devices.sort_by_key(|device| device.name.to_lowercase());
        inner.portals = portals;
        self.state.set_layout(LayoutView {
            displays: local,
            devices,
        });
    }
}

impl Inner {
    fn offset_of(&self, peer_id: &str) -> Option<Offset> {
        self.saved
            .placements
            .iter()
            .find(|record| record.peer_id == peer_id)?
            .offset
    }

    /// The peer's displays in its own space (a default until described).
    fn displays_of(&self, peer_id: &str) -> Vec<Display> {
        layout::peer_or_default(
            self.saved
                .devices
                .get(peer_id)
                .map(|device| device.displays.as_slice())
                .unwrap_or_default(),
        )
    }

    /// Every placed peer, as its displays in this device's space, by ID.
    fn placed(&self) -> Vec<Neighbor> {
        let mut placed: Vec<Neighbor> = self
            .saved
            .placements
            .iter()
            .filter_map(|record| {
                record.offset.map(|offset| Neighbor {
                    id: record.peer_id.clone(),
                    displays: layout::placed(&self.displays_of(&record.peer_id), offset),
                })
            })
            .collect();
        // The core's order, so both snap alike.
        placed.sort_by(|a, b| a.id.cmp(&b.id));
        placed
    }
}

/// Takes a peer's name, platform and displays from a `peer` object of the
/// core's events.
fn describe(device: &mut KnownDevice, id: &str, peer: &Value) {
    device.name = peer["name"].as_str().unwrap_or(id).to_owned();
    if let Some(platform) = peer["platform"].as_str()
        && !platform.is_empty()
    {
        device.platform = platform.to_owned();
    }
    if let Ok(displays) = serde_json::from_value::<Vec<Display>>(peer["displays"].clone())
        && !displays.is_empty()
    {
        device.displays = displays;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    fn screen(x: f64, y: f64, width: f64, height: f64) -> Display {
        Display {
            x,
            y,
            width,
            height,
            scale: 1.0,
        }
    }

    #[test]
    fn placements_from_the_core_become_portals() {
        let directory = std::env::temp_dir().join(format!("rinflow-shell-layout-{}", now_ms()));
        let state = SharedState::new("me", "Me", Settings::default(), false, 42421, "0");
        let arrangement = Arrangement::load(
            &directory,
            state.clone(),
            vec![screen(0.0, 0.0, 1920.0, 1080.0)],
        );
        arrangement.core_event(
            &serde_json::json!({"event": "peer_up", "peer": {"id": "mac", "name": "Mac", "displays": [{"x": 0, "y": 0, "width": 1512, "height": 982, "scale": 2}]}}).to_string(),
        );
        arrangement.core_event(
            &serde_json::json!({"event": "placement_changed", "peer_id": "mac", "offset": {"x": 1920, "y": 0}, "updated_ms": 5}).to_string(),
        );
        let portals = arrangement.portals();
        assert_eq!(portals.len(), 1);
        assert_eq!(portals[0].peer_id, "mac");
        assert_eq!(portals[0].portal.line, 1920.0);
        assert_eq!(state.snapshot().layout.devices[0].offset, Some(Offset { x: 1920.0, y: 0.0 }));
        assert!(arrangement.target("mac").is_some());
        arrangement.forget("mac");
        assert!(arrangement.portals().is_empty());
        let _ = std::fs::remove_dir_all(directory);
    }
}
