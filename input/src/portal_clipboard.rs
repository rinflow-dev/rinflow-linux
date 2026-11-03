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
//! The wire carries three MIME types (`rinflow_core::MIME_*`); this maps
//! them to and from what Wayland applications offer.
//!
//! The two signals are read straight off the bus rather than through
//! ashpd: xdg-desktop-portal 1.20 sends `SelectionOwnerChanged`'s
//! `mime_types` wrapped in a struct, `(as)` where the specification says
//! `as`, and ashpd drops every signal it cannot parse, silently. Both forms
//! are read here.

use crate::{ClipboardCommand, ClipboardEvent};
use ashpd::desktop::Session;
use ashpd::desktop::clipboard::{Clipboard as Portal, RequestClipboardOptions, SetSelectionOptions};
use ashpd::desktop::remote_desktop::RemoteDesktop;
use futures_util::StreamExt;
use std::collections::HashMap;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use rinflow_core::{Clipboard, ClipboardFormat, MAX_CLIPBOARD_SIZE, MIME_HTML, MIME_PNG, MIME_TEXT};
use std::os::fd::OwnedFd;
use std::rc::Rc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

/// How long one application may take to hand over or take one format.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(5);

/// What Wayland applications call UTF-8 text, best first. GTK and Qt offer
/// all of the first four; X11 programs through Xwayland the last two.
const TEXT_TYPES: [&str; 5] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING", "TEXT"];
const HTML_TYPE: &str = "text/html";
const PNG_TYPE: &str = "image/png";
/// Password managers (KeePassXC, KWallet, …) mark what they copy with this
/// type, holding `secret`; such contents never leave this device.
const PASSWORD_HINT: &str = "x-kde-passwordManagerHint";

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
                Some(ClipboardCommand::Enable(on)) => {
                    let was = std::mem::replace(&mut enabled, on);
                    if on && !was && offered.is_none() && !desktop_types.is_empty() {
                        let clipboard = read_shared(&portal, &session, &desktop_types).await;
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
                offered = None;
                desktop_types = change.mime_types;
                log::debug!("clipboard: desktop selection changed: {desktop_types:?}");
                if enabled {
                    let clipboard = read_shared(&portal, &session, &desktop_types).await;
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
                write_selection(&portal, &session, serial, &mime, data).await;
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

/// Reads the formats Rinflow shares out of the desktop's selection, most
/// specific first, within the core's size limit. Empty when a password
/// manager marked it, or nothing shareable is there.
async fn read_shared(portal: &Portal, session: &Session<RemoteDesktop>, types: &[String]) -> Clipboard {
    let has = |name: &str| types.iter().any(|t| t.eq_ignore_ascii_case(name));
    if has(PASSWORD_HINT) {
        let hint = read(portal, session, PASSWORD_HINT, 64).await.unwrap_or_default();
        if String::from_utf8_lossy(&hint).trim() == "secret" {
            log::info!("clipboard: a password manager's secret; not shared");
            return Clipboard::default();
        }
    }
    let mut formats = Vec::new();
    let mut budget = MAX_CLIPBOARD_SIZE.saturating_sub(64);
    let mut take = |mime: &str, data: Vec<u8>, budget: &mut usize| {
        let cost = data.len() + mime.len() + 6;
        if cost <= *budget {
            *budget -= cost;
            formats.push(ClipboardFormat { mime: mime.to_owned(), data });
        } else {
            log::info!("clipboard: {mime} ({} bytes) left out, over the size limit", data.len());
        }
    };
    if has(HTML_TYPE)
        && let Some(html) = read(portal, session, HTML_TYPE, budget).await
        && let Some(text) = decode_text(&html)
        && !text.is_empty()
    {
        take(MIME_HTML, text.into_bytes(), &mut budget);
    }
    if has(PNG_TYPE)
        && let Some(png) = read(portal, session, PNG_TYPE, budget).await
        && png.starts_with(b"\x89PNG\r\n\x1a\n")
    {
        take(MIME_PNG, png, &mut budget);
    }
    if let Some(name) = TEXT_TYPES.iter().find(|name| has(name))
        && let Some(bytes) = read(portal, session, name, budget).await
        && let Some(text) = decode_text(&bytes)
        && !text.is_empty()
    {
        take(MIME_TEXT, text.into_bytes(), &mut budget);
    }
    // Most specific first: HTML, then an image, then plain text. A copied
    // image in a browser comes as both HTML (<img>) and PNG.
    Clipboard { formats }
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

/// The MIME types to announce for a peer's clipboard, in its order.
pub fn local_types(clipboard: &Clipboard) -> Vec<String> {
    let mut types = Vec::new();
    for format in &clipboard.formats {
        let names: &[&str] = match format.mime.as_str() {
            MIME_HTML => &[HTML_TYPE],
            MIME_PNG => &[PNG_TYPE],
            MIME_TEXT => &TEXT_TYPES,
            _ => &[],
        };
        for name in names {
            if !types.iter().any(|t| t == name) {
                types.push((*name).to_owned());
            }
        }
    }
    types
}

/// The bytes to paste for a local MIME type, if the peer's clipboard has them.
pub fn data_for<'a>(clipboard: &'a Clipboard, mime: &str) -> Option<&'a [u8]> {
    let wire = if mime.eq_ignore_ascii_case(HTML_TYPE) || mime.to_ascii_lowercase().starts_with("text/html;") {
        MIME_HTML
    } else if mime.eq_ignore_ascii_case(PNG_TYPE) {
        MIME_PNG
    } else if TEXT_TYPES.iter().any(|t| t.eq_ignore_ascii_case(mime)) {
        MIME_TEXT
    } else {
        return None;
    };
    clipboard
        .formats
        .iter()
        .find(|format| format.mime == wire)
        .map(|format| format.data.as_slice())
}

/// Text as applications hand it over: UTF-8 nearly always; UTF-16 with a
/// byte-order mark from some browsers' HTML. NULs at the end are dropped.
pub fn decode_text(bytes: &[u8]) -> Option<String> {
    let text = if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        String::from_utf16(&units).ok()?
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
        String::from_utf16(&units).ok()?
    } else {
        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
        String::from_utf8_lossy(bytes).into_owned()
    };
    Some(text.trim_end_matches('\0').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clipboard(formats: &[(&str, &[u8])]) -> Clipboard {
        Clipboard {
            formats: formats
                .iter()
                .map(|(mime, data)| ClipboardFormat { mime: (*mime).to_owned(), data: data.to_vec() })
                .collect(),
        }
    }

    #[test]
    fn wire_types_map_to_what_applications_ask_for() {
        let c = clipboard(&[(MIME_HTML, b"<b>hi</b>"), (MIME_TEXT, b"hi"), ("text/rtf", b"{}")]);
        assert_eq!(
            local_types(&c),
            ["text/html", "text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING", "TEXT"]
        );
        assert_eq!(data_for(&c, "UTF8_STRING"), Some(&b"hi"[..]));
        assert_eq!(data_for(&c, "text/plain;charset=UTF-8"), Some(&b"hi"[..]));
        assert_eq!(data_for(&c, "text/html"), Some(&b"<b>hi</b>"[..]));
        assert_eq!(data_for(&c, "image/png"), None);
        assert_eq!(data_for(&c, "text/rtf"), None);
        assert!(local_types(&Clipboard::default()).is_empty());
    }

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

    #[test]
    fn text_decodes_from_what_applications_send() {
        assert_eq!(decode_text("héllo 你好".as_bytes()).unwrap(), "héllo 你好");
        assert_eq!(decode_text(b"\xEF\xBB\xBFbom").unwrap(), "bom");
        assert_eq!(decode_text(b"\xFF\xFEh\0i\0").unwrap(), "hi");
        assert_eq!(decode_text(b"nul\0").unwrap(), "nul");
    }
}
