//! Desktop notifications from the agent, for what needs the user while no
//! window is open: a device asking to pair.
//!
//! Sent straight to `org.freedesktop.Notifications` rather than through
//! the notification portal: the portal tells applications apart by their
//! app ID, which a systemd user service does not have. The `desktop-entry`
//! hint puts the notification under Rinflow's name and icon all the same,
//! and lets the user silence it in the desktop's settings.
//!
//! A pairing request can be opened or declined from its notification, never
//! accepted: accepting means comparing the code on both screens, which is
//! the window's dialog. The notification is withdrawn when the pairing is
//! over, or a window opens (it asks by itself).
//!
//! Like the tray's labels, the text is translated here: nothing else is
//! there to do it.

use crate::services::Services;
use futures_util::StreamExt;
use rinflow_shell::i18n::{self, tr, trf};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use zbus::zvariant::Value;

/// The same device asking again this soon is not announced again.
const SAME_PEER_PAUSE: Duration = Duration::from_secs(30);

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;

    /// The token that lets the application the user chose come forward
    /// (Wayland compositors do not let a background program raise a
    /// window by itself). Sent just before `ActionInvoked`.
    #[zbus(signal)]
    fn activation_token(&self, id: u32, activation_token: String) -> zbus::Result<()>;

    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
}

/// The pairing a notification was shown for: the device and its code (a
/// new code is a new request).
type PairingKey = (String, String);

/// Starts the notifications task. Returns at once; without a session bus
/// or a notification service there are simply none.
pub fn spawn(services: Rc<Services>) {
    tokio::task::spawn_local(async move {
        let bus = match zbus::Connection::session().await {
            Ok(bus) => bus,
            Err(error) => {
                log::info!("no session bus, so no notifications: {error}");
                return;
            }
        };
        let Ok(service) = NotificationsProxy::new(&bus).await else { return };
        let (Ok(mut actions), Ok(mut tokens), Ok(mut closed)) = (
            service.receive_action_invoked().await,
            service.receive_activation_token().await,
            service.receive_notification_closed().await,
        ) else {
            log::info!("cannot listen to the notification service");
            return;
        };

        let mut snapshots = services.state.subscribe();
        let mut windows: watch::Receiver<usize> = services.windows.subscribe();
        // The pairing announced (and under which ID), kept until that
        // pairing is over so a dismissed notification does not come back.
        let mut pairing: Option<(PairingKey, u32)> = None;
        // That notification was taken down because a window opened.
        let mut withdrawn = false;
        let mut announced: HashMap<String, Instant> = HashMap::new();
        let mut activation: HashMap<u32, String> = HashMap::new();
        let mut language = String::new();

        loop {
            // What should be on screen now, against what is.
            let (want, name) = {
                let snapshot = snapshots.borrow_and_update();
                if snapshot.settings.language != language {
                    language = snapshot.settings.language.clone();
                    i18n::set_language(&language);
                }
                let waiting = snapshot.pairing.as_ref().filter(|pairing| !pairing.accepted);
                (
                    waiting.map(|pairing| (pairing.peer_id.clone(), pairing.code.clone())),
                    waiting.map(|pairing| pairing.name.clone()).unwrap_or_default(),
                )
            };
            let window_open = *windows.borrow_and_update() > 0;
            if pairing.as_ref().map(|(key, _)| key) != want.as_ref() {
                if let Some((_, id)) = pairing.take() {
                    let _ = service.close_notification(id).await;
                }
                if let Some(key) = want
                    && !window_open
                    && announced.get(&key.0).is_none_or(|when| when.elapsed() >= SAME_PEER_PAUSE)
                {
                    // Translators: {name} is another device's name.
                    let summary = trf("{name} wants to pair", &[("name", &name)]);
                    let (open, reject) = (tr("Open Rinflow"), tr("Reject"));
                    match show(&service, &summary, &tr("Open Rinflow to compare the pairing code."), &["default", &open, "reject", &reject]).await {
                        Ok(id) => {
                            announced.insert(key.0.clone(), Instant::now());
                            pairing = Some((key, id));
                            withdrawn = false;
                        }
                        Err(error) => log::info!("pairing request not announced: {error}"),
                    }
                }
            } else if window_open && !withdrawn && let Some((_, id)) = &pairing {
                // A window opened: it asks by itself.
                let _ = service.close_notification(*id).await;
                withdrawn = true;
            }

            tokio::select! {
                changed = snapshots.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
                changed = windows.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
                signal = tokens.next() => {
                    let Some(signal) = signal else { break };
                    if let Ok(args) = signal.args() {
                        activation.insert(args.id, args.activation_token);
                    }
                }
                signal = actions.next() => {
                    let Some(signal) = signal else { break };
                    let Ok(args) = signal.args() else { continue };
                    let token = activation.remove(&args.id);
                    if let Some((key, id)) = &pairing && *id == args.id {
                        if args.action_key == "reject" {
                            if let Err(error) = services.reject_pairing(&key.0) {
                                log::warn!("notification: {error}");
                            }
                        } else {
                            crate::tray::open_window(token);
                        }
                    }
                }
                signal = closed.next() => {
                    let Some(signal) = signal else { break };
                    if let Ok(args) = signal.args() {
                        activation.remove(&args.id);
                    }
                }
            }
        }
    });
}

/// Shows one notification under Rinflow's name, which stays until the
/// user answers it.
async fn show(service: &NotificationsProxy<'_>, summary: &str, body: &str, actions: &[&str]) -> zbus::Result<u32> {
    let mut hints = HashMap::new();
    hints.insert("desktop-entry", Value::from(crate::paths::APP_ID));
    // Normal urgency all the same: a request any device on the network can
    // send must not get past "Do Not Disturb".
    service.notify("Rinflow", 0, crate::paths::APP_ID, summary, body, actions, hints, 0).await
}
