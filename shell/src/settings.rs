//! Agent preferences, saved to `settings.json`. Starting at login is not
//! kept here: the platform's autostart mechanism is the truth.

use crate::i18n::{LANGUAGES, Text, msg};
use crate::store;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The discovery port of rinflow-core; the control port must differ.
const DISCOVERY_PORT: u16 = 42420;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Share the keyboard and mouse at all: cross into peers, and let peers
    /// drive this device. Off pauses both; sessions stay.
    pub sharing: bool,
    /// The window's theme: `system`, `light` or `dark`.
    pub theme: String,
    /// The core's control port (UDP); `None` uses the core's default.
    /// Applies after a restart.
    pub port: Option<u16>,
    /// Only paired devices may connect: others pair first (both users check
    /// a code).
    pub paired_only: bool,
    /// What a peer's modifier keys play as while it drives this device (see
    /// rinflow-core `remap.rs`).
    pub modifier_map: rinflow_core::ModifierMap,
    /// Where to look for devices besides the local network's multicast:
    /// host names or IPv4 addresses (see the core's `manual_addresses`).
    pub manual_addresses: Vec<String>,
    /// How this device's mouse feels on each peer it drives, by device ID.
    pub peer_input: BTreeMap<String, PeerInput>,
    /// The window's language: `system`, `zh-CN`, `ja-JP` or `en-US`.
    pub language: String,
    /// Share the clipboard with peers as control changes hands (text, HTML,
    /// images). Off by default, as on Windows.
    pub clipboard: bool,
    /// Share copied files as well: files copied on one device can be
    /// pasted on the next. Needs `clipboard`; off by default.
    pub files: bool,
    /// How long the pointer must keep pushing against an edge before it
    /// crosses, in milliseconds (0: at once). Against crossing by accident.
    pub switch_delay_ms: u32,
    /// Edge hits this close to a screen corner (logical points) do not
    /// cross: corners are where menus and hot corners live. 0: none.
    pub corner_guard: u32,
    /// Scroll Lock, pressed while driving another device, keeps the
    /// pointer on it (or lets it go again).
    pub scroll_lock_locks: bool,
}

/// The longest `switch_delay_ms`, and the largest `corner_guard`.
pub const MAX_SWITCH_DELAY_MS: u32 = 2000;
pub const MAX_CORNER_GUARD: u32 = 400;

/// Pointer and scrolling on one peer, applied by this device as it drives it
/// (it tracks the pointer there, so the peer must not change the motion).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PeerInput {
    /// Multiplies pointer motion: 0.25 to 4.
    pub pointer_speed: f64,
    /// Multiplies scrolling: 0.25 to 5.
    pub scroll_speed: f64,
    /// Scrolls the other way (natural scrolling on one side only).
    pub invert_scroll: bool,
}

impl Default for PeerInput {
    fn default() -> Self {
        Self {
            pointer_speed: 1.0,
            scroll_speed: 1.0,
            invert_scroll: false,
        }
    }
}

impl PeerInput {
    fn is_valid(&self) -> bool {
        (0.25..=4.0).contains(&self.pointer_speed) && (0.25..=5.0).contains(&self.scroll_speed)
    }
}

/// Addresses the user may add, at most (the core's limit).
pub const MAX_MANUAL_ADDRESSES: usize = 32;

impl Default for Settings {
    fn default() -> Self {
        Self {
            sharing: true,
            theme: "system".to_owned(),
            port: None,
            paired_only: true,
            modifier_map: rinflow_core::ModifierMap::SWAP,
            manual_addresses: Vec::new(),
            peer_input: BTreeMap::new(),
            language: "system".to_owned(),
            clipboard: false,
            files: false,
            switch_delay_ms: 0,
            corner_guard: 0,
            scroll_lock_locks: true,
        }
    }
}

impl Settings {
    /// Applies the given keys to a copy; unknown keys and invalid values are
    /// refused with a message for the user.
    pub fn merged(&self, values: &Map<String, Value>) -> Result<Self, Text> {
        let mut object = match serde_json::to_value(self) {
            Ok(Value::Object(object)) => object,
            _ => return Err(msg("Settings couldn't be read")),
        };
        for (key, value) in values {
            if !object.contains_key(key) {
                return Err(msg("Unknown setting: {key}").arg("key", key.as_str()));
            }
            object.insert(key.clone(), value.clone());
        }
        let merged: Self = serde_json::from_value(Value::Object(object)).map_err(|error| {
            msg("Invalid setting value: {error}").arg("error", error.to_string())
        })?;
        if !matches!(merged.theme.as_str(), "system" | "light" | "dark") {
            return Err(msg("Theme must be system, light, or dark"));
        }
        if !LANGUAGES.contains(&merged.language.as_str()) {
            return Err(msg("Language must be system, Simplified Chinese, Japanese, or English"));
        }
        if let Some(port) = merged.port
            && (port < 1024 || port == DISCOVERY_PORT)
        {
            return Err(msg("Ports must be between 1024 and 65535 and not the discovery port {port}")
                .arg("port", DISCOVERY_PORT.to_string()));
        }
        if merged.manual_addresses.len() > MAX_MANUAL_ADDRESSES {
            return Err(msg("You can add up to {count} addresses").arg("count", MAX_MANUAL_ADDRESSES.to_string()));
        }
        if let Some(bad) = merged
            .manual_addresses
            .iter()
            .find(|address| rinflow_core::parse_address(address, DISCOVERY_PORT).is_err())
        {
            return Err(msg("“{address}” isn't a valid address. Enter an IP address or a device name.").arg("address", bad.as_str()));
        }
        if merged.switch_delay_ms > MAX_SWITCH_DELAY_MS {
            return Err(msg("The crossing delay can be at most {max} ms").arg("max", MAX_SWITCH_DELAY_MS.to_string()));
        }
        if merged.corner_guard > MAX_CORNER_GUARD {
            return Err(msg("The corner margin can be at most {max} points").arg("max", MAX_CORNER_GUARD.to_string()));
        }
        if merged.peer_input.values().any(|input| !input.is_valid()) {
            return Err(msg("Pointer speed must be between 0.25× and 4×, and scroll speed between 0.25× and 5×"));
        }
        Ok(merged)
    }

    /// How this device's mouse feels on `peer_id`.
    pub fn input_for(&self, peer_id: &str) -> PeerInput {
        self.peer_input.get(peer_id).copied().unwrap_or_default()
    }
}

/// The settings and their file. Reading never waits for the disk: input
/// paths read settings. The file is written outside the lock, and only ever
/// with the newest settings.
pub struct SettingsStore {
    path: PathBuf,
    /// The settings, and how many changes they have had.
    current: Mutex<(Settings, u64)>,
    /// Taken while saving: the change count of the settings last saved.
    saved: Mutex<u64>,
}

impl SettingsStore {
    pub fn load(directory: &Path) -> Self {
        let path = directory.join("settings.json");
        let settings: Settings = store::read_json(&path).unwrap_or_default();
        // A file edited by hand falls back to defaults for what is invalid.
        let settings = settings
            .merged(&Map::new())
            .unwrap_or_else(|_| Settings::default());
        Self {
            path,
            current: Mutex::new((settings, 0)),
            saved: Mutex::new(0),
        }
    }

    pub fn get(&self) -> Settings {
        self.current
            .lock()
            .map(|current| current.0.clone())
            .unwrap_or_default()
    }

    pub fn update(&self, change: impl FnOnce(&mut Settings)) -> Settings {
        let (settings, changes) = {
            let Ok(mut current) = self.current.lock() else {
                return Settings::default();
            };
            change(&mut current.0);
            current.1 += 1;
            (current.0.clone(), current.1)
        };
        // Saves take turns. One that comes after a save of newer settings
        // has nothing to add, and must not put older ones back.
        if let Ok(mut saved) = self.saved.lock()
            && *saved < changes
        {
            store::save_json(&self.path, "settings", &settings);
            *saved = changes;
        }
        settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_refuses_unknown_keys_and_bad_values() {
        let settings = Settings::default();
        assert!(settings.merged(&Map::new()).is_ok());
        let mut values = Map::new();
        values.insert("nonsense".into(), Value::Bool(true));
        assert!(settings.merged(&values).is_err());
        let mut values = Map::new();
        values.insert("port".into(), Value::from(80));
        assert!(settings.merged(&values).is_err());
        let mut values = Map::new();
        values.insert("port".into(), Value::from(50000));
        assert_eq!(settings.merged(&values).unwrap().port, Some(50000));
        let mut values = Map::new();
        values.insert("manual_addresses".into(), serde_json::json!(["192.168.1.20"]));
        assert_eq!(settings.merged(&values).unwrap().manual_addresses.len(), 1);
        assert!(!settings.clipboard);
        let mut values = Map::new();
        values.insert("clipboard".into(), Value::Bool(true));
        assert!(settings.merged(&values).unwrap().clipboard);
        values.insert("clipboard".into(), Value::from("yes"));
        assert!(settings.merged(&values).is_err());
    }
}
