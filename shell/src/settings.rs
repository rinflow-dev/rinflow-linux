//! Agent preferences, saved to `settings.json`. Starting at login is not
//! kept here: the platform's autostart mechanism is the truth.

use crate::i18n::{self, LANGUAGES, tr};
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
    /// The language of the agent's own text: `system`, `zh-CN` or `en-US`.
    pub language: String,
    /// Share the clipboard with peers as control changes hands (text, HTML,
    /// images). Off by default, as on Windows.
    pub clipboard: bool,
}

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
        }
    }
}

impl Settings {
    /// Applies the given keys to a copy; unknown keys and invalid values are
    /// refused with a message for the user.
    pub fn merged(&self, values: &Map<String, Value>) -> Result<Self, String> {
        let mut object = match serde_json::to_value(self) {
            Ok(Value::Object(object)) => object,
            _ => return Err(tr("设置无法读取", "Settings couldn't be read").to_owned()),
        };
        for (key, value) in values {
            if !object.contains_key(key) {
                return Err(if i18n::chinese() {
                    format!("未知的设置项：{key}")
                } else {
                    format!("Unknown setting: {key}")
                });
            }
            object.insert(key.clone(), value.clone());
        }
        let merged: Self = serde_json::from_value(Value::Object(object)).map_err(|error| {
            if i18n::chinese() {
                format!("设置的值无效：{error}")
            } else {
                format!("Invalid setting value: {error}")
            }
        })?;
        if !matches!(merged.theme.as_str(), "system" | "light" | "dark") {
            return Err(tr(
                "主题只能是跟随系统、浅色或深色",
                "Theme must be system, light, or dark",
            )
            .to_owned());
        }
        if !LANGUAGES.contains(&merged.language.as_str()) {
            return Err(tr(
                "语言只能是跟随系统、简体中文或英语",
                "Language must be system, Simplified Chinese, or English",
            )
            .to_owned());
        }
        if let Some(port) = merged.port
            && (port < 1024 || port == DISCOVERY_PORT)
        {
            return Err(if i18n::chinese() {
                format!("端口需在 1024–65535 之间，且不能是发现端口 {DISCOVERY_PORT}")
            } else {
                format!(
                    "Ports must be between 1024 and 65535 and not the discovery port {DISCOVERY_PORT}"
                )
            });
        }
        if merged.manual_addresses.len() > MAX_MANUAL_ADDRESSES {
            return Err(if i18n::chinese() {
                format!("最多添加 {MAX_MANUAL_ADDRESSES} 个地址")
            } else {
                format!("You can add up to {MAX_MANUAL_ADDRESSES} addresses")
            });
        }
        if let Some(bad) = merged
            .manual_addresses
            .iter()
            .find(|address| rinflow_core::parse_address(address, DISCOVERY_PORT).is_err())
        {
            return Err(if i18n::chinese() {
                format!("“{bad}”不是有效的地址：请输入 IP 地址或设备名称")
            } else {
                format!("“{bad}” isn't a valid address. Enter an IP address or a device name.")
            });
        }
        if merged.peer_input.values().any(|input| !input.is_valid()) {
            return Err(tr(
                "指针速度需在 0.25–4 倍之间，滚动速度在 0.25–5 倍之间",
                "Pointer speed must be between 0.25× and 4×, and scroll speed between 0.25× and 5×",
            )
            .to_owned());
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
