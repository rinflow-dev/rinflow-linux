//! What the window can drive through IPC.

use crate::autostart;
use rinflow_core::Core;
use rinflow_core::layout::Offset;
use rinflow_shell::arrangement::Arrangement;
use rinflow_shell::i18n::{self, tr};
use rinflow_shell::settings::SettingsStore;
use rinflow_shell::state::SharedState;
use serde_json::{Map, Value};
use std::sync::{Arc, Weak};
use tokio::sync::mpsc;

/// What the main loop does on the window's behalf, because it owns the
/// backends.
#[derive(Debug)]
pub enum AgentCommand {
    /// Settings changed; keys named so the loop applies what concerns it.
    SettingsChanged(Vec<String>),
    RetryInput,
    Quit,
}

pub struct Services {
    pub core: Weak<Core>,
    pub state: SharedState,
    pub settings: Arc<SettingsStore>,
    pub arrangement: Arc<Arrangement>,
    pub commands: mpsc::UnboundedSender<AgentCommand>,
}

impl Services {
    /// Changes some settings (keys of `Settings`, plus `launch_at_login`),
    /// applies them at once and remembers them.
    pub fn update_settings(&self, mut values: Map<String, Value>) -> Result<(), String> {
        if let Some(value) = values.remove("launch_at_login") {
            let enabled = value.as_bool().ok_or(tr(
                "launch_at_login 需要 true 或 false",
                "launch_at_login must be true or false",
            ))?;
            autostart::set(enabled).map_err(|error| {
                if i18n::chinese() {
                    format!("无法设置登录时启动：{error}")
                } else {
                    format!("Couldn't change \"Start when I sign in\": {error}")
                }
            })?;
            self.state.set_launch_at_login(autostart::enabled());
        }
        if values.is_empty() {
            return Ok(());
        }
        let merged = self.settings.get().merged(&values)?;
        let settings = self.settings.update(|settings| *settings = merged);
        let keys: Vec<String> = values.keys().cloned().collect();

        if let Some(core) = self.core.upgrade() {
            if values.contains_key("sharing") {
                let _ = core.set_remote_control_allowed(settings.sharing);
            }
            if values.contains_key("paired_only") {
                let _ = core.set_require_pairing(settings.paired_only);
            }
            if values.contains_key("modifier_map") {
                let _ = core.set_modifier_map(settings.modifier_map);
            }
            if values.contains_key("manual_addresses") {
                let _ = core.set_manual_addresses(settings.manual_addresses.clone());
            }
        }
        if values.contains_key("language") {
            i18n::set_language(&settings.language);
        }
        self.state.set_settings(settings);
        let _ = self.commands.send(AgentCommand::SettingsChanged(keys));
        Ok(())
    }

    pub fn preview_placement(&self, peer_id: &str, offset: Offset) -> Option<Offset> {
        self.arrangement.preview(peer_id, offset)
    }

    /// Forgets a device: disconnected, unplaced, and no longer listed as one
    /// of the user's devices.
    pub fn forget_device(&self, peer_id: &str) -> Result<(), String> {
        if let Some(core) = self.core.upgrade() {
            let _ = core.disconnect_peer(peer_id);
            let _ = core.set_placement(peer_id, None);
            let _ = core.unpair(peer_id);
        }
        self.arrangement.forget(peer_id);
        Ok(())
    }

    pub fn accept_pairing(&self, peer_id: &str) -> Result<(), String> {
        let core = self.core()?;
        core.accept_pairing(peer_id)?;
        self.state.set_pairing_accepted(peer_id);
        Ok(())
    }

    pub fn reject_pairing(&self, peer_id: &str) -> Result<(), String> {
        self.core()?.reject_pairing(peer_id)
    }

    pub fn core(&self) -> Result<Arc<Core>, String> {
        self.core
            .upgrade()
            .ok_or_else(|| tr("核心正在关闭", "The core is shutting down").to_owned())
    }
}
