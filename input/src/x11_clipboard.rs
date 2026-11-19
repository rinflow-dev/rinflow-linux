//! The desktop's clipboard on an X11 session: the CLIPBOARD selection, by
//! the conventions every X program follows (ICCCM). Any program may use
//! it, so it needs no permission and is there whenever the X server is.
//!
//! - **Out:** the XFixes extension says when another program takes the
//!   selection; the formats Rinflow shares are then read at once and
//!   reported as [`ClipboardEvent::Changed`], as on Wayland.
//! - **In:** a peer's clipboard is offered by owning the selection; the
//!   data goes out when a program pastes (a `SelectionRequest`).
//!
//! Which formats are shared, and what programs call them, is in
//! `clipboard_formats.rs`: X programs name formats by MIME type too, GTK
//! and Qt among them.
//!
//! The parts of the convention that matter in practice, as Deskflow's
//! XWindowsClipboard.cpp has them: `TARGETS` (some programs answer it with
//! the wrong type, which is accepted), `TIMESTAMP`, `MULTIPLE`, and
//! `INCR`, by which anything too large for one request moves in pieces,
//! in both directions. Its Motif clipboard is left out: nothing in use
//! speaks it.

use crate::clipboard_formats::{GIVE_UP_ECHO, GNOME_COPIED_FILES, TRANSFER_TIMEOUT, URI_LIST, data_for, local_types, read_shared};
use crate::x11_common::{Link, X11Display};
use crate::{ClipboardCommand, ClipboardEvent};
use rinflow_core::Clipboard;
use std::collections::{HashMap, VecDeque};
use std::fmt::Display as Shown;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use x11rb::connection::Connection;
use x11rb::protocol::xfixes::{self, SelectionEventMask};
use x11rb::protocol::xproto::{self, Atom, AtomEnum, ConnectionExt as _, PropMode, Property, Window};
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;

/// The most one request carries; anything larger goes in pieces (`INCR`).
const LARGEST_PIECE: usize = 256 * 1024;

/// Serves the clipboard until `commands` closes (the emulation backend
/// ends) or the X server goes away.
pub(crate) async fn serve(
    display: X11Display,
    mut commands: mpsc::UnboundedReceiver<ClipboardCommand>,
    events: mpsc::UnboundedSender<ClipboardEvent>,
) {
    let mut board = match Board::open(&display) {
        Ok(board) => board,
        Err(error) => {
            log::warn!("clipboard: {error}");
            return;
        }
    };
    let _ = events.send(ClipboardEvent::Available(true));
    log::info!("clipboard: ready");

    let result: Result<(), String> = async {
        // What is on the clipboard already counts as copied.
        board.owner_changed(None, &events).await?;
        loop {
            loop {
                let event = match board.backlog.pop_front() {
                    Some(event) => event,
                    None => match board.link.conn.poll_for_event().map_err(text)? {
                        Some(event) => event,
                        None => break,
                    },
                };
                board.handle(event, &events).await?;
            }
            board.sends.retain(|send| send.deadline > Instant::now());
            board.link.conn.flush().map_err(text)?;
            tokio::select! {
                command = commands.recv() => match command {
                    None => return Ok(()),
                    Some(command) => board.command(command, &events).await?,
                },
                readable = board.link.readable() => readable.map_err(text)?,
                // A program that stopped taking its pieces is forgotten.
                _ = tokio::time::sleep(Duration::from_secs(1)), if !board.sends.is_empty() => {}
            }
        }
    }
    .await;
    if let Err(error) = result {
        log::warn!("clipboard: X server: {error}");
    }
    let _ = events.send(ClipboardEvent::Available(false));
    log::info!("clipboard: ended");
}

fn text(error: impl Shown) -> String {
    error.to_string()
}

struct Atoms {
    clipboard: Atom,
    targets: Atom,
    multiple: Atom,
    timestamp: Atom,
    incr: Atom,
    atom_pair: Atom,
    utf8_string: Atom,
    /// The property on this backend's window that conversions land in.
    landing: Atom,
    /// The property touched to learn the server's time.
    clock: Atom,
}

/// One format on its way, in pieces, to a program that pasted.
struct Send {
    requestor: Window,
    property: Atom,
    kind: Atom,
    data: Vec<u8>,
    sent: usize,
    deadline: Instant,
}

struct Board {
    link: Link,
    window: Window,
    atoms: Atoms,
    /// A peer's clipboard, offered while this backend owns the selection,
    /// and the server's time when it took it.
    offered: Option<(Clipboard, u32)>,
    /// The formats the desktop's own selection had when it last changed,
    /// by the name and the atom its owner gave them.
    desktop: Vec<(String, Atom)>,
    enabled: bool,
    files: bool,
    /// When this backend last gave the selection up (`WithdrawFiles`): the
    /// server then says the selection has no owner, which is not another
    /// program's doing.
    gave_up: Option<Instant>,
    /// Events that arrived while a conversion was waited for and were not
    /// part of it.
    backlog: VecDeque<Event>,
    /// Another program took the selection while it was being read: what is
    /// being read is stale, and the rest of it is skipped.
    superseded: bool,
    sends: Vec<Send>,
    names: HashMap<Atom, String>,
    interned: HashMap<String, Atom>,
}

impl Board {
    fn open(display: &X11Display) -> Result<Self, String> {
        let link = Link::open(display)?;
        let conn = &link.conn;
        xfixes::query_version(conn, 5, 0).map_err(text)?.reply().map_err(|_| "the XFixes extension is missing".to_owned())?;
        let window = conn.generate_id().map_err(text)?;
        conn.create_window(
            0,
            window,
            link.root,
            -10,
            -10,
            1,
            1,
            0,
            xproto::WindowClass::INPUT_ONLY,
            x11rb::COPY_FROM_PARENT,
            &xproto::CreateWindowAux::new().override_redirect(1).event_mask(xproto::EventMask::PROPERTY_CHANGE),
        )
        .map_err(text)?;
        let atom = |name: &str| -> Result<Atom, String> { Ok(conn.intern_atom(false, name.as_bytes()).map_err(text)?.reply().map_err(text)?.atom) };
        let atoms = Atoms {
            clipboard: atom("CLIPBOARD")?,
            targets: atom("TARGETS")?,
            multiple: atom("MULTIPLE")?,
            timestamp: atom("TIMESTAMP")?,
            incr: atom("INCR")?,
            atom_pair: atom("ATOM_PAIR")?,
            utf8_string: atom("UTF8_STRING")?,
            landing: atom("RINFLOW_SELECTION")?,
            clock: atom("RINFLOW_CLOCK")?,
        };
        xfixes::select_selection_input(
            conn,
            window,
            atoms.clipboard,
            SelectionEventMask::SET_SELECTION_OWNER | SelectionEventMask::SELECTION_WINDOW_DESTROY | SelectionEventMask::SELECTION_CLIENT_CLOSE,
        )
        .map_err(text)?;
        Ok(Self {
            link,
            window,
            atoms,
            offered: None,
            desktop: Vec::new(),
            enabled: false,
            files: false,
            gave_up: None,
            backlog: VecDeque::new(),
            superseded: false,
            sends: Vec::new(),
            names: HashMap::new(),
            interned: HashMap::new(),
        })
    }

    async fn command(&mut self, command: ClipboardCommand, events: &mpsc::UnboundedSender<ClipboardEvent>) -> Result<(), String> {
        match command {
            ClipboardCommand::Offer(clipboard) => {
                let types = local_types(&clipboard);
                if types.is_empty() {
                    return Ok(());
                }
                let time = self.server_time().await?;
                let conn = &self.link.conn;
                conn.set_selection_owner(self.window, self.atoms.clipboard, time).map_err(text)?;
                let owner = conn.get_selection_owner(self.atoms.clipboard).map_err(text)?.reply().map_err(text)?.owner;
                if owner == self.window {
                    log::info!("clipboard: offering a peer's clipboard as {types:?}");
                    self.offered = Some((clipboard, time));
                } else {
                    log::warn!("clipboard: the X server did not give the selection");
                }
            }
            ClipboardCommand::WithdrawFiles => {
                if self.offered.as_ref().is_some_and(|(clipboard, _)| clipboard.formats.iter().any(|format| format.mime == URI_LIST)) {
                    self.offered = None;
                    self.link.conn.set_selection_owner(x11rb::NONE, self.atoms.clipboard, x11rb::CURRENT_TIME).map_err(text)?;
                    log::info!("clipboard: a peer's files are gone; emptied");
                    self.gave_up = Some(Instant::now());
                }
            }
            ClipboardCommand::Enable(on) => {
                let was = std::mem::replace(&mut self.enabled, on);
                if on && !was && self.offered.is_none() && !self.desktop.is_empty() {
                    self.report_desktop(events).await?;
                }
            }
            ClipboardCommand::Files(on) => {
                let was = std::mem::replace(&mut self.files, on);
                // Files already copied are shared (or no longer) at once.
                let copied = self.desktop.iter().any(|(name, _)| name == URI_LIST || name == GNOME_COPIED_FILES);
                if on != was && self.enabled && self.offered.is_none() && copied {
                    self.report_desktop(events).await?;
                }
            }
        }
        Ok(())
    }

    async fn handle(&mut self, event: Event, events: &mpsc::UnboundedSender<ClipboardEvent>) -> Result<(), String> {
        match event {
            Event::XfixesSelectionNotify(change) if change.selection == self.atoms.clipboard => {
                self.owner_changed(Some(change.owner), events).await?;
            }
            Event::SelectionRequest(request) => self.answer(&request)?,
            Event::PropertyNotify(change) if change.state == Property::DELETE => self.send_on(change.window, change.atom)?,
            Event::Error(error) => log::debug!("clipboard: the server refused a request: {error:?}"),
            _ => {}
        }
        Ok(())
    }

    /// The selection has a new owner (`None`: look who it is), or none.
    async fn owner_changed(&mut self, owner: Option<Window>, events: &mpsc::UnboundedSender<ClipboardEvent>) -> Result<(), String> {
        self.superseded = false;
        let owner = match owner {
            Some(owner) => owner,
            None => self.link.conn.get_selection_owner(self.atoms.clipboard).map_err(text)?.reply().map_err(text)?.owner,
        };
        if owner == self.window {
            // Our own offer coming back.
            return Ok(());
        }
        self.offered = None;
        if owner == x11rb::NONE {
            self.desktop.clear();
            if self.gave_up.take().is_some_and(|when| when.elapsed() < GIVE_UP_ECHO) {
                // Our own giving up coming back.
                return Ok(());
            }
            if self.enabled {
                let _ = events.send(ClipboardEvent::Changed(Clipboard::default()));
            }
            return Ok(());
        }
        self.desktop = self.targets().await?;
        log::debug!("clipboard: desktop selection changed: {:?}", self.desktop.iter().map(|(name, _)| name).collect::<Vec<_>>());
        if self.enabled && !self.superseded {
            self.report_desktop(events).await?;
        }
        Ok(())
    }

    /// Reads what the desktop's selection shares and reports it.
    async fn report_desktop(&mut self, events: &mpsc::UnboundedSender<ClipboardEvent>) -> Result<(), String> {
        let types: Vec<String> = self.desktop.iter().map(|(name, _)| name.clone()).collect();
        let files = self.files;
        let mut failure = None;
        let clipboard = read_shared(&types, files, async |mime, limit| match self.read(mime, limit).await {
            Ok(data) => data,
            Err(error) => {
                failure.get_or_insert(error);
                None
            }
        })
        .await;
        if let Some(error) = failure {
            return Err(error);
        }
        if self.superseded {
            // Copied over while it was read; the new one is read next.
            return Ok(());
        }
        let _ = events.send(ClipboardEvent::Changed(clipboard));
        Ok(())
    }

    /// The formats the selection's owner offers, by name.
    async fn targets(&mut self) -> Result<Vec<(String, Atom)>, String> {
        let Some(reply) = self.convert(self.atoms.targets, 64 * 1024).await? else {
            return Ok(Vec::new());
        };
        // The type should be ATOM; some owners say TARGETS. Either way it
        // is a list of 32-bit atoms.
        let atoms: Vec<Atom> = reply.data.as_chunks::<4>().0.iter().map(|chunk| u32::from_ne_bytes(*chunk)).filter(|atom| *atom != x11rb::NONE).collect();
        if reply.format != 32 {
            return Ok(Vec::new());
        }
        let mut targets = Vec::with_capacity(atoms.len());
        for atom in atoms {
            if let Some(name) = self.name(atom)? {
                targets.push((name, atom));
            }
        }
        Ok(targets)
    }

    /// One format of the desktop's selection, at most `limit` bytes.
    async fn read(&mut self, mime: &str, limit: usize) -> Result<Option<Vec<u8>>, String> {
        let Some(target) = self.desktop.iter().find(|(name, _)| name.eq_ignore_ascii_case(mime)).map(|(_, atom)| *atom) else {
            return Ok(None);
        };
        let Some(reply) = self.convert(target, limit).await? else {
            return Ok(None);
        };
        // The oldest text type is Latin-1 by definition; everything else
        // that is text is UTF-8 (or says so itself, see `decode_text`).
        Ok(Some(if reply.kind == Atom::from(AtomEnum::STRING) { latin1_to_utf8(&reply.data) } else { reply.data }))
    }

    /// Asks the selection's owner for `target` and waits for it: `None`
    /// when the owner refuses, does not answer in time, has more than
    /// `limit` bytes, or lost the selection meanwhile.
    async fn convert(&mut self, target: Atom, limit: usize) -> Result<Option<Converted>, String> {
        if self.superseded {
            return Ok(None);
        }
        let (window, landing, selection) = (self.window, self.atoms.landing, self.atoms.clipboard);
        let conn = &self.link.conn;
        conn.delete_property(window, landing).map_err(text)?;
        conn.convert_selection(window, selection, target, landing, x11rb::CURRENT_TIME).map_err(text)?;
        let mut deadline = Instant::now() + TRANSFER_TIMEOUT;
        loop {
            match self.next_event(deadline).await? {
                None => {
                    log::warn!("clipboard: the program holding the clipboard did not hand over {}", self.name(target)?.unwrap_or_default());
                    return Ok(None);
                }
                Some(Event::SelectionNotify(notify)) if notify.requestor == window && notify.selection == selection => {
                    if notify.property == x11rb::NONE {
                        return Ok(None);
                    }
                    break;
                }
                Some(event) => {
                    if self.defer(event) {
                        return Ok(None);
                    }
                }
            }
        }
        let long_length = u32::try_from(limit / 4 + 1).unwrap_or(u32::MAX);
        let first = self.link.conn.get_property(true, window, landing, AtomEnum::ANY, 0, long_length).map_err(text)?.reply().map_err(text)?;
        if first.type_ != self.atoms.incr {
            if first.bytes_after > 0 {
                log::info!("clipboard: {} is larger than {limit} bytes; not shared", self.name(target)?.unwrap_or_default());
                self.link.conn.delete_property(window, landing).map_err(text)?;
                return Ok(None);
            }
            return Ok(Some(Converted { kind: first.type_, format: first.format, data: first.value }));
        }
        // In pieces: reading the property above (and deleting it) told the
        // owner to start. Each piece arrives as a new value of the same
        // property; an empty one ends it. Too much is read to the end all
        // the same, and dropped, so the owner is not left half-way.
        log::debug!("clipboard: {} comes in pieces", self.name(target)?.unwrap_or_default());
        let mut data = Vec::new();
        let mut kind = x11rb::NONE;
        let mut format = 8;
        let mut fits = true;
        deadline = Instant::now() + TRANSFER_TIMEOUT;
        loop {
            match self.next_event(deadline).await? {
                None => {
                    log::warn!("clipboard: the program holding the clipboard stopped handing over {}", self.name(target)?.unwrap_or_default());
                    return Ok(None);
                }
                Some(Event::PropertyNotify(change)) if change.window == window && change.atom == landing => {
                    if change.state != Property::NEW_VALUE {
                        continue;
                    }
                    let piece = self.link.conn.get_property(true, window, landing, AtomEnum::ANY, 0, u32::MAX / 4).map_err(text)?.reply().map_err(text)?;
                    if piece.value.is_empty() {
                        break;
                    }
                    (kind, format) = (piece.type_, piece.format);
                    if fits && data.len() + piece.value.len() <= limit {
                        data.extend_from_slice(&piece.value);
                    } else {
                        fits = false;
                        data = Vec::new();
                    }
                    deadline = Instant::now() + TRANSFER_TIMEOUT;
                }
                Some(event) => {
                    if self.defer(event) {
                        return Ok(None);
                    }
                }
            }
        }
        if !fits {
            log::info!("clipboard: {} is larger than {limit} bytes; not shared", self.name(target)?.unwrap_or_default());
            return Ok(None);
        }
        Ok(Some(Converted { kind, format, data }))
    }

    /// Keeps an event that is not part of the conversion being waited for,
    /// for the main loop. True when it means the selection changed hands,
    /// which ends the conversion.
    fn defer(&mut self, event: Event) -> bool {
        let taken = matches!(&event, Event::XfixesSelectionNotify(change) if change.selection == self.atoms.clipboard && change.owner != self.window);
        match event {
            // Of no use later.
            Event::PropertyNotify(change) if change.window == self.window => {}
            Event::SelectionNotify(_) => {}
            event => self.backlog.push_back(event),
        }
        if taken {
            self.superseded = true;
        }
        taken
    }

    /// The next event from the server, or `None` at `deadline`.
    async fn next_event(&mut self, deadline: Instant) -> Result<Option<Event>, String> {
        loop {
            if let Some(event) = self.link.conn.poll_for_event().map_err(text)? {
                return Ok(Some(event));
            }
            self.link.conn.flush().map_err(text)?;
            match tokio::time::timeout_at(deadline.into(), self.link.readable()).await {
                Ok(readable) => readable.map_err(text)?,
                Err(_) => return Ok(None),
            }
        }
    }

    /// The server's clock, which a selection is to be taken with: read off
    /// the event for a change to a property of our own.
    async fn server_time(&mut self) -> Result<u32, String> {
        let (window, clock) = (self.window, self.atoms.clock);
        self.link.conn.change_property8(PropMode::APPEND, window, clock, AtomEnum::STRING, &[]).map_err(text)?;
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match self.next_event(deadline).await? {
                None => return Ok(x11rb::CURRENT_TIME),
                Some(Event::PropertyNotify(change)) if change.window == window && change.atom == clock => return Ok(change.time),
                Some(event) => {
                    self.defer(event);
                }
            }
        }
    }

    /// A program pastes: it asks for one format of what is offered.
    fn answer(&mut self, request: &xproto::SelectionRequestEvent) -> Result<(), String> {
        // Programs from before the convention was settled name no property.
        let property = if request.property == x11rb::NONE { request.target } else { request.property };
        let supplied = request.selection == self.atoms.clipboard && self.offered.is_some() && self.supply(request.requestor, request.target, property)?;
        let notify = xproto::SelectionNotifyEvent {
            response_type: xproto::SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if supplied { property } else { x11rb::NONE },
        };
        self.link.conn.send_event(false, request.requestor, xproto::EventMask::NO_EVENT, notify).map_err(text)?;
        Ok(())
    }

    /// Puts `target` of the offered clipboard into `property` of the
    /// requestor's window. False when it is not offered.
    fn supply(&mut self, requestor: Window, target: Atom, property: Atom) -> Result<bool, String> {
        let Some((clipboard, time)) = &self.offered else { return Ok(false) };
        let time = *time;
        if target == self.atoms.targets {
            let names = local_types(clipboard);
            let mut atoms = vec![self.atoms.targets, self.atoms.timestamp, self.atoms.multiple];
            for name in &names {
                atoms.push(self.intern(name)?);
            }
            self.link.conn.change_property32(PropMode::REPLACE, requestor, property, AtomEnum::ATOM, &atoms).map_err(text)?;
            return Ok(true);
        }
        if target == self.atoms.timestamp {
            self.link.conn.change_property32(PropMode::REPLACE, requestor, property, AtomEnum::INTEGER, &[time]).map_err(text)?;
            return Ok(true);
        }
        if target == self.atoms.multiple {
            // Several at once: the property lists (target, property)
            // pairs; a pair that cannot be had gets no property.
            let Ok(list) = self.link.conn.get_property(false, requestor, property, AtomEnum::ANY, 0, 1024).map_err(text)?.reply() else {
                return Ok(false);
            };
            let mut pairs: Vec<Atom> = list.value32().map(Iterator::collect).unwrap_or_default();
            let mut refused = false;
            for pair in pairs.as_chunks_mut::<2>().0 {
                if pair[0] == self.atoms.multiple || !self.supply(requestor, pair[0], pair[1])? {
                    pair[1] = x11rb::NONE;
                    refused = true;
                }
            }
            if refused {
                self.link.conn.change_property32(PropMode::REPLACE, requestor, property, self.atoms.atom_pair, &pairs).map_err(text)?;
            }
            return Ok(true);
        }
        let Some(name) = self.name(target)? else { return Ok(false) };
        let Some((clipboard, _)) = &self.offered else { return Ok(false) };
        let Some(data) = data_for(clipboard, &name) else {
            log::debug!("clipboard: asked for {name}, which is not offered");
            return Ok(false);
        };
        // STRING is Latin-1 by definition; TEXT is answered in UTF-8 and
        // says so; the rest are what they are called.
        let (kind, data) = match name.as_str() {
            "STRING" => (target, utf8_to_latin1(&data)),
            "TEXT" => (self.atoms.utf8_string, data.into_owned()),
            _ => (target, data.into_owned()),
        };
        let conn = &self.link.conn;
        if data.len() <= LARGEST_PIECE {
            log::debug!("clipboard: pasted {name} ({} bytes)", data.len());
            conn.change_property8(PropMode::REPLACE, requestor, property, kind, &data).map_err(text)?;
            return Ok(true);
        }
        // In pieces: the property says so and how much is coming; each
        // time the program deletes it, the next piece goes in.
        log::debug!("clipboard: pasting {name} ({} bytes) in pieces", data.len());
        conn.change_window_attributes(requestor, &xproto::ChangeWindowAttributesAux::new().event_mask(xproto::EventMask::PROPERTY_CHANGE)).map_err(text)?;
        conn.change_property32(PropMode::REPLACE, requestor, property, self.atoms.incr, &[u32::try_from(data.len()).unwrap_or(u32::MAX)]).map_err(text)?;
        self.sends.retain(|send| !(send.requestor == requestor && send.property == property));
        self.sends.push(Send { requestor, property, kind, data, sent: 0, deadline: Instant::now() + TRANSFER_TIMEOUT });
        Ok(true)
    }

    /// The program took a piece (it deleted the property): the next one,
    /// or an empty one after the last.
    fn send_on(&mut self, requestor: Window, property: Atom) -> Result<(), String> {
        let Some(index) = self.sends.iter().position(|send| send.requestor == requestor && send.property == property) else {
            return Ok(());
        };
        let conn = &self.link.conn;
        let send = &mut self.sends[index];
        let piece = &send.data[send.sent..(send.sent + LARGEST_PIECE).min(send.data.len())];
        conn.change_property8(PropMode::REPLACE, requestor, property, send.kind, piece).map_err(text)?;
        send.sent += piece.len();
        send.deadline = Instant::now() + TRANSFER_TIMEOUT;
        if piece.is_empty() {
            self.sends.remove(index);
            if !self.sends.iter().any(|send| send.requestor == requestor) {
                conn.change_window_attributes(requestor, &xproto::ChangeWindowAttributesAux::new().event_mask(xproto::EventMask::NO_EVENT)).map_err(text)?;
            }
        }
        Ok(())
    }

    /// An atom's name; `None` for one the server does not know.
    fn name(&mut self, atom: Atom) -> Result<Option<String>, String> {
        if let Some(name) = self.names.get(&atom) {
            return Ok(Some(name.clone()));
        }
        let Ok(reply) = self.link.conn.get_atom_name(atom).map_err(text)?.reply() else {
            return Ok(None);
        };
        let name = String::from_utf8_lossy(&reply.name).into_owned();
        self.names.insert(atom, name.clone());
        Ok(Some(name))
    }

    fn intern(&mut self, name: &str) -> Result<Atom, String> {
        if let Some(atom) = self.interned.get(name) {
            return Ok(*atom);
        }
        let atom = self.link.conn.intern_atom(false, name.as_bytes()).map_err(text)?.reply().map_err(text)?.atom;
        self.interned.insert(name.to_owned(), atom);
        self.names.insert(atom, name.to_owned());
        Ok(atom)
    }
}

/// What a conversion put into the landing property.
struct Converted {
    kind: Atom,
    format: u8,
    data: Vec<u8>,
}

fn latin1_to_utf8(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(|byte| char::from(*byte)).collect::<String>().into_bytes()
}

/// Latin-1 has no place for most characters: those become `?`.
fn utf8_to_latin1(bytes: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(bytes).chars().map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_oldest_text_type_is_latin_1() {
        assert_eq!(latin1_to_utf8(b"caf\xE9"), "café".as_bytes());
        assert_eq!(utf8_to_latin1("café".as_bytes()), b"caf\xE9");
        assert_eq!(utf8_to_latin1("你好 ok".as_bytes()), b"?? ok");
    }
}
