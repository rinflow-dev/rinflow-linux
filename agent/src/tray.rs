//! The tray icon (a StatusNotifierItem, over D-Bus; no toolkit): what
//! Rinflow is doing at a glance, and the two switches worth reaching
//! without the window: pause sharing, keep the pointer on this screen.
//!
//! Plasma, Steam Deck's Desktop Mode, Cinnamon, Xfce and GNOME with the
//! AppIndicator extension show it. Where nothing does (plain GNOME, Gaming
//! Mode) it waits for a host to appear and costs nothing meanwhile.
//!
//! The agent's messages for the window go out untranslated; the tray is
//! the one thing the agent shows by itself, so its few labels are
//! translated here, from the same catalogs, following the same setting.

use crate::services::{AgentCommand, Services};
use ksni::TrayMethods;
use ksni::menu::{CheckmarkItem, MenuItem, StandardItem};
use rinflow_shell::i18n::{self, tr, trf};
use rinflow_shell::state::{SessionStatus, Snapshot};
use std::rc::Rc;
use tokio::sync::mpsc;

/// What the tray shows, taken from a snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
struct View {
    sharing: bool,
    locked: bool,
    /// The name of the device being driven, if any.
    driving: Option<String>,
    /// The name of a device driving this one, if any.
    driven_by: Option<String>,
    connected: usize,
    language: String,
}

impl View {
    fn of(snapshot: &Snapshot) -> Self {
        let name = |peer_id: &str| {
            snapshot
                .sessions
                .iter()
                .find(|session| session.peer_id == peer_id)
                .map(|session| session.name.clone())
                .unwrap_or_else(|| peer_id.to_owned())
        };
        Self {
            sharing: snapshot.settings.sharing,
            locked: snapshot.locked,
            driving: snapshot.driving.as_deref().map(name),
            driven_by: snapshot.sessions.iter().find(|session| session.remote_control).map(|session| session.name.clone()),
            connected: snapshot.sessions.iter().filter(|session| session.status == SessionStatus::Connected).count(),
            language: snapshot.settings.language.clone(),
        }
    }

    /// One line: what is going on right now.
    fn summary(&self) -> String {
        if let Some(name) = &self.driving {
            // Translators: {name} is another device's name.
            trf("Controlling {name}", &[("name", name)])
        } else if let Some(name) = &self.driven_by {
            // Translators: {name} is another device's name.
            trf("Controlled by {name}", &[("name", name)])
        } else if !self.sharing {
            tr("Sharing is paused")
        } else if self.connected == 0 {
            tr("No devices connected")
        } else if self.connected == 1 {
            tr("1 device connected")
        } else {
            // Translators: {count} is a number, 2 or more.
            trf("{count} devices connected", &[("count", &self.connected)])
        }
    }
}

enum Action {
    SetSharing(bool),
    SetLocked(bool),
    OpenWindow,
    Quit,
}

struct RinflowTray {
    view: View,
    actions: mpsc::UnboundedSender<Action>,
}

impl ksni::Tray for RinflowTray {
    fn id(&self) -> String {
        crate::paths::APP_ID.to_owned()
    }

    fn title(&self) -> String {
        "Rinflow".to_owned()
    }

    fn icon_name(&self) -> String {
        format!("{}-symbolic", crate::paths::APP_ID)
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: "Rinflow".to_owned(), description: self.view.summary(), ..Default::default() }
    }

    /// A click on the icon opens the window.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.actions.send(Action::OpenWindow);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem { label: self.view.summary(), enabled: false, ..Default::default() }.into(),
            MenuItem::Separator,
            CheckmarkItem {
                label: tr("Share mouse and keyboard"),
                checked: self.view.sharing,
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.actions.send(Action::SetSharing(!tray.view.sharing));
                }),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: tr("Keep the pointer on this screen"),
                checked: self.view.locked,
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.actions.send(Action::SetLocked(!tray.view.locked));
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: tr("Open Rinflow"),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.actions.send(Action::OpenWindow);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: tr("Quit"),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.actions.send(Action::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Starts the tray and keeps it in step with the agent's state. Returns at
/// once; without a session bus there is simply no tray.
pub fn spawn(services: Rc<Services>) {
    tokio::task::spawn_local(async move {
        let mut snapshots = services.state.subscribe();
        let view = View::of(&snapshots.borrow_and_update());
        i18n::set_language(&view.language);
        let (actions, mut requested) = mpsc::unbounded_channel();
        let tray = RinflowTray { view: view.clone(), actions };
        // In a sandbox the well-known name cannot be owned; and a host
        // that is not there yet (the desktop comes after the agent) is
        // waited for rather than an error.
        let sandboxed = std::path::Path::new("/.flatpak-info").exists();
        let handle = match tray.disable_dbus_name(sandboxed).assume_sni_available(true).spawn().await {
            Ok(handle) => handle,
            Err(error) => {
                log::info!("no tray icon: {error}");
                return;
            }
        };
        let mut shown = view;
        loop {
            tokio::select! {
                changed = snapshots.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let view = View::of(&snapshots.borrow_and_update());
                    if view != shown {
                        if view.language != shown.language {
                            i18n::set_language(&view.language);
                        }
                        shown = view.clone();
                        handle.update(move |tray| tray.view = view).await;
                    }
                }
                action = requested.recv() => match action {
                    Some(Action::SetSharing(on)) => {
                        let mut values = serde_json::Map::new();
                        values.insert("sharing".to_owned(), on.into());
                        if let Err(error) = services.update_settings(values) {
                            log::warn!("tray: {error}");
                        }
                    }
                    Some(Action::SetLocked(on)) => {
                        services.state.set_locked(Some(on));
                        let _ = services.commands.send(AgentCommand::LockChanged);
                    }
                    Some(Action::OpenWindow) => open_window(),
                    Some(Action::Quit) => {
                        let _ = services.commands.send(AgentCommand::Quit);
                    }
                    None => break,
                },
            }
        }
    });
}

/// Starts the window (or brings the running one forward: it is a single
/// instance application).
fn open_window() {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|directory| directory.join("rinflow")))
        .filter(|path| path.is_file());
    let program = beside.unwrap_or_else(|| "rinflow".into());
    match std::process::Command::new(&program)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(error) => log::warn!("cannot open the window ({}): {error}", program.display()),
    }
}
