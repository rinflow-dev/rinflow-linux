//! The desktop's clipboard through `org.freedesktop.portal.Clipboard`, which
//! rides on the RemoteDesktop session the emulation backend already holds:
//! the portal grants it with that session (asked for before `Start`), and
//! it lives and dies with it. GNOME (≥ 45) and Plasma (≥ 6.2) offer it.
//!
//! - **Out:** when another application takes the selection, the formats
//!   Rinflow shares are read at once (`SelectionRead`) and reported as
//!   [`ClipboardEvent::Changed`]: the core sends them to a peer only when
//!   control changes hands, and then without asking the desktop again.
//! - **In:** a peer's clipboard is announced as this session's selection
//!   (`SetSelection`); the data goes out when an application pastes
//!   (`SelectionTransfer` → `SelectionWrite`).
//!
//! Which formats are shared, and what applications call them, is in
//! `clipboard_formats.rs`. Copied files are among them, as `text/uri-list`:
//! read here as the file manager offers it (the core turns the paths into
//! a manifest; they never leave this device), and, for files a peer
//! offers, offered to file managers with paths the agent makes for them
//! (`remote_files.rs`).
//!
//! The two signals are read straight off the bus rather than through
//! ashpd: xdg-desktop-portal 1.20 sends `SelectionOwnerChanged`'s
//! `mime_types` wrapped in a struct, `(as)` where the specification says
//! `as`, and ashpd drops every signal it cannot parse, silently. Both forms
//! are read here.

use crate::clipboard_formats::{GIVE_UP_ECHO, GNOME_COPIED_FILES, TRANSFER_TIMEOUT, URI_LIST, data_for, local_types, read_shared};
use crate::{ClipboardCommand, ClipboardEvent};
use ashpd::desktop::Session;
use ashpd::desktop::clipboard::{Clipboard as Portal, RequestClipboardOptions, SetSelectionOptions};
use ashpd::desktop::remote_desktop::RemoteDesktop;
use futures_util::StreamExt;
use std::collections::HashMap;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use rinflow_core::Clipboard;
use std::os::fd::OwnedFd;
use std::rc::Rc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

/// Asks for the clipboard on `session`; must come before the session's
/// `Start`. `None` when the desktop has no clipboard portal or refuses.
pub async fn request(session: &Session<RemoteDesktop>) -> Option<Portal> {
    let portal = match Portal::new().await {
        Ok(portal) => portal,
        Err(error) => {
            log::info!("no clipboard portal: {error}");
            return None;
        }
    };
    match portal.request(session, RequestClipboardOptions::default()).await {
        Ok(()) => Some(portal),
        Err(error) => {
            log::info!("clipboard not available: {error}");
            None
        }
    }
}

/// Serves the clipboard of `session` until `commands` closes (the
/// emulation backend ends) or the portal goes away.
pub async fn serve(
    portal: Portal,
    session: Rc<Session<RemoteDesktop>>,
    mut commands: mpsc::UnboundedReceiver<ClipboardCommand>,
    events: mpsc::UnboundedSender<ClipboardEvent>,
) {
    let bus = portal.connection().clone();
    let (mut owner_changes, mut transfers) =
        match (signals(&bus, "SelectionOwnerChanged").await, signals(&bus, "SelectionTransfer").await) {
            (Ok(owner), Ok(transfer)) => (owner, transfer),
            (Err(error), _) | (_, Err(error)) => {
                log::warn!("clipboard: cannot listen to the portal's signals: {error}");
                return;
            }
        };
    let ours = session_path(&session);
    let _ = events.send(ClipboardEvent::Available(true));
    log::info!("clipboard: ready");

    // What this session offers applications (a peer's clipboard), and the
    // types the desktop's own selection had when it last changed.
    let mut offered: Option<Clipboard> = None;
    let mut desktop_types: Vec<String> = Vec::new();
    let mut enabled = false;
    let mut files = false;
    // When this session last gave the selection up (`WithdrawFiles`). The
    // desktop then says that the selection has no owner, and may say it
    // after the next offer was made: taken for another application's
    // copy, that would drop the offer just made, and nothing would be
    // pasted. (A peer that copies files twice in a row does exactly this:
    // its old files go, its new ones come.)
    let mut gave_up: Option<Instant> = None;

    loop {
        tokio::select! {
            command = commands.recv() => match command {
                None => break,
                Some(ClipboardCommand::Offer(clipboard)) => {
                    let types = local_types(&clipboard);
                    if types.is_empty() {
                        continue;
                    }
                    let names: Vec<&str> = types.iter().map(String::as_str).collect();
                    match portal.set_selection(&session, SetSelectionOptions::default().set_mime_types(&names)).await {
                        Ok(()) => {
                            log::info!("clipboard: offering a peer's clipboard as {names:?}");
                            offered = Some(clipboard);
                        }
                        Err(error) => log::warn!("clipboard: SetSelection: {error}"),
                    }
                }
                Some(ClipboardCommand::WithdrawFiles) => {
                    if offered.as_ref().is_some_and(|clipboard| clipboard.formats.iter().any(|format| format.mime == URI_LIST)) {
                        offered = None;
                        // Without a list of types the selection is given
                        // up (Mutter refuses an empty list).
                        match portal.set_selection(&session, SetSelectionOptions::default()).await {
                            Ok(()) => {
                                log::info!("clipboard: a peer's files are gone; emptied");
                                gave_up = Some(Instant::now());
                            }
                            Err(error) => log::warn!("clipboard: SetSelection: {error}"),
                        }
                    }
                }
                Some(ClipboardCommand::Enable(on)) => {
                    let was = std::mem::replace(&mut enabled, on);
                    if on && !was && offered.is_none() && !desktop_types.is_empty() {
                        let clipboard = read_shared(&desktop_types, files, async |mime, limit| read(&portal, &session, mime, limit).await).await;
                        let _ = events.send(ClipboardEvent::Changed(clipboard));
                    }
                }
                Some(ClipboardCommand::Files(on)) => {
                    let was = std::mem::replace(&mut files, on);
                    // Files already copied are shared (or no longer) at once.
                    let copied = desktop_types.iter().any(|t| t == URI_LIST || t == GNOME_COPIED_FILES);
                    if on != was && enabled && offered.is_none() && copied {
                        let clipboard = read_shared(&desktop_types, files, async |mime, limit| read(&portal, &session, mime, limit).await).await;
                        let _ = events.send(ClipboardEvent::Changed(clipboard));
                    }
                }
            },
            message = owner_changes.next() => {
                let Some(message) = message else { break };
                let Some(change) = message.ok().and_then(|m| OwnerChange::parse(&m)) else { continue };
                if !ours.as_ref().is_none_or(|ours| *ours == change.session) {
                    continue;
                }
                if change.session_is_owner {
                    // Our own SetSelection coming back.
                    continue;
                }
                if change.mime_types.is_empty() && gave_up.take().is_some_and(|when| when.elapsed() < GIVE_UP_ECHO) {
                    // Our own giving up coming back: what is offered now,
                    // if anything, was offered after it.
                    desktop_types.clear();
                    continue;
                }
                offered = None;
                desktop_types = change.mime_types;
                log::debug!("clipboard: desktop selection changed: {desktop_types:?}");
                if enabled {
                    let clipboard = read_shared(&desktop_types, files, async |mime, limit| read(&portal, &session, mime, limit).await).await;
                    let _ = events.send(ClipboardEvent::Changed(clipboard));
                }
            }
            message = transfers.next() => {
                let Some(message) = message else { break };
                let Some((from, mime, serial)) = message
                    .ok()
                    .and_then(|m| m.body().deserialize::<(OwnedObjectPath, String, u32)>().ok())
                else {
                    continue;
                };
                if !ours.as_ref().is_none_or(|ours| ours.as_str() == from.as_str()) {
                    continue;
                }
                let data = offered.as_ref().and_then(|clipboard| data_for(clipboard, &mime));
                write_selection(&portal, &session, serial, &mime, data.as_deref()).await;
            }
        }
    }
    let _ = events.send(ClipboardEvent::Available(false));
    log::info!("clipboard: ended");
}

/// The portal's clipboard signals: on the portal's object, from the
/// portal (the bus resolves the well-known name to its owner).
async fn signals(bus: &zbus::Connection, member: &'static str) -> zbus::Result<zbus::MessageStream> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.portal.Desktop")?
        .interface("org.freedesktop.portal.Clipboard")?
        .member(member)?
        .path("/org/freedesktop/portal/desktop")?
        .build();
    zbus::MessageStream::for_match_rule(rule, bus, None).await
}

/// ashpd keeps a session's object path to itself; its `Debug` shows it, as
/// `Session("/org/freedesktop/portal/desktop/session/…")`. `None` if that
/// ever changes: the signals are sent to this connection alone anyway.
fn session_path(session: &Session<RemoteDesktop>) -> Option<String> {
    let text = format!("{session:?}");
    let start = text.find('"')? + 1;
    let end = text.rfind('"')?;
    (end > start).then(|| text[start..end].to_owned())
}

/// One `SelectionOwnerChanged`.
#[derive(Debug, PartialEq)]
struct OwnerChange {
    session: String,
    mime_types: Vec<String>,
    session_is_owner: bool,
}

impl OwnerChange {
    fn parse(message: &zbus::Message) -> Option<Self> {
        let (session, options) = message
            .body()
            .deserialize::<(OwnedObjectPath, HashMap<String, OwnedValue>)>()
            .ok()?;
        Some(Self::from_options(session.as_str(), &options))
    }

    fn from_options(session: &str, options: &HashMap<String, OwnedValue>) -> Self {
        let mime_types = options
            .get("mime_types")
            .and_then(|value| {
                // `as` as specified, or `(as)` as xdg-desktop-portal 1.20 sends it.
                Vec::<String>::try_from(value.try_clone().ok()?)
                    .ok()
                    .or_else(|| <(Vec<String>,)>::try_from(value.try_clone().ok()?).ok().map(|(types,)| types))
            })
            .unwrap_or_default();
        let session_is_owner = options
            .get("session_is_owner")
            .and_then(|value| bool::try_from(value).ok())
            .unwrap_or(false);
        Self { session: session.to_owned(), mime_types, session_is_owner }
    }
}

/// One format of the desktop's selection, at most `limit` bytes; `None`
/// when it is larger, the application does not answer, or reading fails.
async fn read(portal: &Portal, session: &Session<RemoteDesktop>, mime: &str, limit: usize) -> Option<Vec<u8>> {
    let fd = match portal.selection_read(session, mime).await {
        Ok(fd) => OwnedFd::from(fd),
        Err(error) => {
            log::debug!("clipboard: SelectionRead {mime}: {error}");
            return None;
        }
    };
    let mut pipe = match tokio::net::unix::pipe::Receiver::from_owned_fd(fd) {
        Ok(pipe) => pipe,
        Err(error) => {
            log::warn!("clipboard: {mime}: {error}");
            return None;
        }
    };
    let mut data = Vec::new();
    let reading = async {
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let n = pipe.read(&mut chunk).await?;
            if n == 0 {
                return Ok::<bool, std::io::Error>(true);
            }
            if data.len() + n > limit {
                return Ok(false);
            }
            data.extend_from_slice(&chunk[..n]);
        }
    };
    match tokio::time::timeout(TRANSFER_TIMEOUT, reading).await {
        Ok(Ok(true)) => Some(data),
        Ok(Ok(false)) => {
            log::info!("clipboard: {mime} is larger than {limit} bytes; not shared");
            None
        }
        Ok(Err(error)) => {
            log::warn!("clipboard: reading {mime}: {error}");
            None
        }
        Err(_) => {
            log::warn!("clipboard: the application holding the clipboard did not hand over {mime}");
            None
        }
    }
}

/// Hands one format of the offered clipboard to an application that pastes.
async fn write_selection(portal: &Portal, session: &Session<RemoteDesktop>, serial: u32, mime: &str, data: Option<&[u8]>) {
    let Some(data) = data else {
        log::debug!("clipboard: asked for {mime}, which is not offered");
        let _ = portal.selection_write_done(session, serial, false).await;
        return;
    };
    let fd = match portal.selection_write(session, serial).await {
        Ok(fd) => OwnedFd::from(fd),
        Err(error) => {
            log::warn!("clipboard: SelectionWrite: {error}");
            let _ = portal.selection_write_done(session, serial, false).await;
            return;
        }
    };
    let written = async {
        let mut pipe = tokio::net::unix::pipe::Sender::from_owned_fd(fd)?;
        pipe.write_all(data).await?;
        pipe.flush().await
    };
    let success = match tokio::time::timeout(TRANSFER_TIMEOUT, written).await {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            log::warn!("clipboard: writing {mime}: {error}");
            false
        }
        Err(_) => {
            log::warn!("clipboard: the pasting application did not take {mime}");
            false
        }
    };
    log::debug!("clipboard: pasted {mime} ({} bytes, {success})", data.len());
    if let Err(error) = portal.selection_write_done(session, serial, success).await {
        log::warn!("clipboard: SelectionWriteDone: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_changes_parse_in_both_forms_portals_send() {
        use zbus::zvariant::{Structure, Value};
        let types = vec!["text/plain".to_owned(), "text/html".to_owned()];
        let mut options = HashMap::new();
        options.insert("mime_types".to_owned(), OwnedValue::try_from(Value::from(types.clone())).unwrap());
        options.insert("session_is_owner".to_owned(), OwnedValue::from(true));
        let change = OwnerChange::from_options("/s", &options);
        assert_eq!(change.mime_types, types);
        assert!(change.session_is_owner);
        // xdg-desktop-portal 1.20: the list wrapped in a struct, no owner flag.
        let wrapped = Value::Structure(Structure::from((types.clone(),)));
        let mut options = HashMap::new();
        options.insert("mime_types".to_owned(), OwnedValue::try_from(wrapped).unwrap());
        let change = OwnerChange::from_options("/s", &options);
        assert_eq!(change.mime_types, types);
        assert!(!change.session_is_owner);
    }
}
