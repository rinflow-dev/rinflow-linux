//! What the X11 backends share: connecting to the X server the agent was
//! told about, waiting for its events on the agent's own event loop, and
//! the desktop's scale.
//!
//! The connection is x11rb's, which reads and writes without blocking the
//! thread for long: a request with a reply waits for the server (a local
//! round trip), everything that waits on another program (a selection's
//! owner, the user) goes through [`Link::readable`].
//!
//! Units: the X server speaks pixels, the core logical points. X11 has one
//! scale for the whole desktop, which the desktop publishes for toolkits
//! in one of two places: GTK's window scale in XSETTINGS (Xfce, Cinnamon,
//! MATE and GNOME set it, and leave `Xft.dpi` at 96 or not), and `Xft.dpi`
//! in the resource database (Plasma, and whoever sets it by hand). The
//! larger of the two is the scale; a point is that many pixels, on every
//! monitor.

use rinflow_core::layout::Display;
use std::cell::{Cell, OnceCell};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::randr;
use x11rb::protocol::xinput::Fp3232;
use x11rb::protocol::xproto::{self, Atom, AtomEnum, ConnectionExt as _, Window};
use x11rb::rust_connection::{DefaultStream, RustConnection};
use x11rb::reexports::x11rb_protocol::parse_display::parse_display;
use x11rb::reexports::x11rb_protocol::xauth::Family;

/// The X server of the session: `DISPLAY`, and the `XAUTHORITY` that goes
/// with it (both from the same environment, the agent's own or the systemd
/// user manager's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct X11Display {
    pub name: String,
    pub authority: Option<PathBuf>,
}

/// One connection to the X server.
pub(crate) struct Link {
    /// First, so it leaves the event loop before the connection closes
    /// the socket it watches.
    fd: AsyncFd<Fd>,
    pub conn: RustConnection,
    pub root: Window,
    screen: usize,
    settings: OnceCell<Option<Settings>>,
}

/// Where the desktop's settings daemon publishes XSETTINGS: it owns a
/// selection, and keeps the settings in a property of the owning window.
struct Settings {
    selection: Atom,
    property: Atom,
    /// The type of the message a daemon sends when it takes the selection.
    manager: Atom,
    /// The window that owns the selection now; `NONE` while no daemon runs.
    owner: Cell<Window>,
}

/// The connection's socket, for the event loop to watch. x11rb owns and
/// closes it.
struct Fd(RawFd);

impl AsRawFd for Fd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl Link {
    pub fn open(display: &X11Display) -> Result<Self, String> {
        let (conn, screen) = connect(display).map_err(|error| format!("X server {}: {error}", display.name))?;
        let root = conn.setup().roots[screen].root;
        // SAFETY: the descriptor is the connection's socket, open for as
        // long as `conn` lives, and `conn` outlives `fd`: both go into the
        // same `Link`, which drops `fd` first.
        let fd = unsafe { AsyncFd::register_with_interest(Fd(conn.stream().as_raw_fd()), Interest::READABLE) }
            .map_err(|error| format!("X server {}: {}", display.name, io::Error::from(error)))?;
        Ok(Self { fd, conn, root, screen, settings: OnceCell::new() })
    }

    /// Waits until the server has sent something. Events x11rb read while
    /// it waited for a reply are not announced here: callers take every
    /// queued event (`poll_for_event`) before they wait.
    pub async fn readable(&self) -> io::Result<()> {
        loop {
            let mut guard = self.fd.readable().await?;
            if has_input(self.fd.get_ref().0)? {
                // Left marked readable: x11rb may not take all of it at once.
                return Ok(());
            }
            guard.clear_ready();
        }
    }

    /// Pixels per point: GTK's window scale or `Xft.dpi` over 96 (in
    /// quarter steps: a text size a little off 96 is not a scale),
    /// whichever is larger.
    pub fn scale(&self) -> f64 {
        let resources = self
            .conn
            .get_property(false, self.root, AtomEnum::RESOURCE_MANAGER, AtomEnum::STRING, 0, 1 << 18)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| String::from_utf8_lossy(&reply.value).into_owned())
            .unwrap_or_default();
        let window_scale = self
            .settings()
            .filter(|settings| settings.owner.get() != x11rb::NONE)
            .and_then(|settings| self.conn.get_property(false, settings.owner.get(), settings.property, AtomEnum::ANY, 0, 1 << 16).ok())
            .and_then(|cookie| cookie.reply().ok())
            .and_then(|reply| xsettings_integer(&reply.value, "Gdk/WindowScalingFactor"))
            .map_or(1.0, f64::from);
        scale_of(&resources).max(window_scale).clamp(1.0, 4.0)
    }

    fn settings(&self) -> Option<&Settings> {
        self.settings
            .get_or_init(|| {
                let atom = |name: String| self.conn.intern_atom(false, name.as_bytes()).ok()?.reply().ok().map(|reply| reply.atom);
                let settings = Settings {
                    selection: atom(format!("_XSETTINGS_S{}", self.screen))?,
                    property: atom("_XSETTINGS_SETTINGS".to_owned())?,
                    manager: atom("MANAGER".to_owned())?,
                    owner: Cell::new(x11rb::NONE),
                };
                self.find_settings_owner(&settings);
                Some(settings)
            })
            .as_ref()
    }

    /// Looks up the settings daemon's window and asks to hear when its
    /// settings change.
    fn find_settings_owner(&self, settings: &Settings) {
        let owner = self.conn.get_selection_owner(settings.selection).ok().and_then(|cookie| cookie.reply().ok()).map_or(x11rb::NONE, |reply| reply.owner);
        if owner != x11rb::NONE {
            let _ = self.conn.change_window_attributes(owner, &xproto::ChangeWindowAttributesAux::new().event_mask(xproto::EventMask::PROPERTY_CHANGE));
        }
        settings.owner.set(owner);
    }

    /// The monitors in points, as RandR lists them (the active ones); the
    /// whole screen as one where RandR has no monitors to tell of.
    pub fn displays(&self, scale: f64) -> Vec<Display> {
        let mut pixels: Vec<(i32, i32, u32, u32)> = randr::get_monitors(&self.conn, self.root, true)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| {
                reply
                    .monitors
                    .iter()
                    .map(|monitor| (i32::from(monitor.x), i32::from(monitor.y), u32::from(monitor.width), u32::from(monitor.height)))
                    .filter(|(_, _, width, height)| *width > 0 && *height > 0)
                    .collect()
            })
            .unwrap_or_default();
        // Mirrored monitors show the same part of the desktop.
        let mut seen = Vec::new();
        pixels.retain(|monitor| {
            let new = !seen.contains(monitor);
            seen.push(*monitor);
            new
        });
        if pixels.is_empty() {
            let screen = &self.conn.setup().roots[self.screen];
            pixels.push((0, 0, u32::from(screen.width_in_pixels), u32::from(screen.height_in_pixels)));
        }
        pixels
            .into_iter()
            .map(|(x, y, width, height)| Display {
                x: f64::from(x) / scale,
                y: f64::from(y) / scale,
                width: f64::from(width) / scale,
                height: f64::from(height) / scale,
                scale,
            })
            .collect()
    }

    /// Asks to hear when monitors come, go or change, and when the scale
    /// does: [`Link::concerns_geometry`] tells which events those are.
    pub fn watch_geometry(&self) {
        // Version 1.5 has the monitors; an older server answers with the
        // whole screen (see `displays`).
        let _ = randr::query_version(&self.conn, 1, 5).map(|cookie| cookie.reply());
        let _ = randr::select_input(&self.conn, self.root, randr::NotifyMask::SCREEN_CHANGE | randr::NotifyMask::CRTC_CHANGE);
        // The resource database is a property of the root window; a
        // settings daemon that starts later says so in a message to it.
        let _ = self.conn.change_window_attributes(
            self.root,
            &xproto::ChangeWindowAttributesAux::new().event_mask(xproto::EventMask::PROPERTY_CHANGE | xproto::EventMask::STRUCTURE_NOTIFY),
        );
        let _ = self.settings();
    }

    /// Whether an event means the displays or the scale may have changed.
    pub fn concerns_geometry(&self, event: &Event) -> bool {
        match event {
            Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_) => true,
            Event::PropertyNotify(change) => {
                (change.window == self.root && change.atom == Atom::from(AtomEnum::RESOURCE_MANAGER))
                    || self.settings().is_some_and(|settings| change.window == settings.owner.get() && change.atom == settings.property)
            }
            // A settings daemon came up (the agent may be there before the
            // desktop is): from now on its window is the one to watch.
            Event::ClientMessage(message) => self.settings().is_some_and(|settings| {
                let announced = message.type_ == settings.manager && message.format == 32 && message.data.as_data32()[1] == settings.selection;
                if announced {
                    self.find_settings_owner(settings);
                }
                announced
            }),
            _ => false,
        }
    }
}

fn has_input(fd: RawFd) -> io::Result<bool> {
    let mut poll = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    // SAFETY: `poll` points at one valid pollfd for the length given.
    let ready = unsafe { libc::poll(&mut poll, 1, 0) };
    if ready < 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::Interrupted { Ok(true) } else { Err(error) };
    }
    Ok(ready > 0)
}

/// As `RustConnection::connect`, but with the authority file named by the
/// environment the display came from rather than this process's own.
fn connect(display: &X11Display) -> Result<(RustConnection, usize), String> {
    let parsed = parse_display(Some(&display.name)).map_err(|error| error.to_string())?;
    let screen = usize::from(parsed.screen);
    let authority = display
        .authority
        .clone()
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".Xauthority")));
    let mut last = "no address to connect to".to_owned();
    for address in parsed.connect_instruction() {
        let (stream, (family, peer)) = match DefaultStream::connect(&address) {
            Ok(connected) => connected,
            Err(error) => {
                last = error.to_string();
                continue;
            }
        };
        let (name, data) = authority
            .as_deref()
            .and_then(|path| cookie(path, family, &peer, parsed.display))
            .unwrap_or_default();
        return RustConnection::connect_to_stream_with_auth_info(stream, screen, name, data)
            .map(|conn| (conn, screen))
            .map_err(|error| error.to_string());
    }
    Err(last)
}

/// The `MIT-MAGIC-COOKIE-1` of an authority file for this server, as
/// libxcb picks it: the first entry whose address and display number match
/// (or are left open).
fn cookie(path: &Path, family: Family, address: &[u8], display: u16) -> Option<(Vec<u8>, Vec<u8>)> {
    let bytes = std::fs::read(path).ok()?;
    let number = display.to_string();
    authority_entries(&bytes)
        .into_iter()
        .find(|entry| {
            let entry_family = Family::from(entry.family);
            (family == Family::WILD || entry_family == Family::WILD || (entry_family == family && entry.address == address))
                && (entry.number.is_empty() || entry.number == number.as_bytes())
                && entry.name == b"MIT-MAGIC-COOKIE-1"
        })
        .map(|entry| (entry.name.to_vec(), entry.data.to_vec()))
}

struct AuthorityEntry<'a> {
    family: u16,
    address: &'a [u8],
    number: &'a [u8],
    name: &'a [u8],
    data: &'a [u8],
}

/// The entries of an `.Xauthority` file: a big-endian family, then four
/// counted strings (address, display number, protocol name, data). What
/// follows a malformed entry is left out.
fn authority_entries(mut bytes: &[u8]) -> Vec<AuthorityEntry<'_>> {
    fn word(bytes: &mut &[u8]) -> Option<u16> {
        let (head, rest) = bytes.split_first_chunk::<2>()?;
        *bytes = rest;
        Some(u16::from_be_bytes(*head))
    }
    fn counted<'a>(bytes: &mut &'a [u8]) -> Option<&'a [u8]> {
        let length = usize::from(word(bytes)?);
        let (head, rest) = bytes.split_at_checked(length)?;
        *bytes = rest;
        Some(head)
    }
    let mut entries = Vec::new();
    while !bytes.is_empty() {
        let entry = (|| {
            Some(AuthorityEntry {
                family: word(&mut bytes)?,
                address: counted(&mut bytes)?,
                number: counted(&mut bytes)?,
                name: counted(&mut bytes)?,
                data: counted(&mut bytes)?,
            })
        })();
        match entry {
            Some(entry) => entries.push(entry),
            None => break,
        }
    }
    entries
}

/// The scale in an X resource database (`Xft.dpi: 192` is 2).
pub(crate) fn scale_of(resources: &str) -> f64 {
    let dpi = resources
        .lines()
        .find_map(|line| line.trim().strip_prefix("Xft.dpi:"))
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|dpi| dpi.is_finite() && *dpi > 0.0)
        .unwrap_or(96.0);
    ((dpi / 96.0 * 4.0).round() / 4.0).clamp(1.0, 4.0)
}

/// An integer setting out of an XSETTINGS property: a byte-order mark, a
/// serial and a count, then the settings, each a type (0 integer, 1
/// string, 2 colour), a name and a value, padded to four bytes.
pub(crate) fn xsettings_integer(data: &[u8], wanted: &str) -> Option<i32> {
    let big_endian = *data.first()? == 1;
    let short = |at: usize| data.get(at..at + 2).map(|bytes| if big_endian { u16::from_be_bytes([bytes[0], bytes[1]]) } else { u16::from_le_bytes([bytes[0], bytes[1]]) });
    let long = |at: usize| {
        data.get(at..at + 4).map(|bytes| {
            let bytes = [bytes[0], bytes[1], bytes[2], bytes[3]];
            if big_endian { u32::from_be_bytes(bytes) } else { u32::from_le_bytes(bytes) }
        })
    };
    let padded = |length: usize| length.div_ceil(4) * 4;
    let mut at = 12;
    for _ in 0..long(8)? {
        let kind = *data.get(at)?;
        let name_length = usize::from(short(at + 2)?);
        let name = data.get(at + 4..at + 4 + name_length)?;
        // The name, then the serial of its last change.
        at += 4 + padded(name_length) + 4;
        match kind {
            0 => {
                let value = long(at)? as i32;
                if name == wanted.as_bytes() {
                    return Some(value);
                }
                at += 4;
            }
            1 => at += 4 + padded(usize::try_from(long(at)?).ok()?),
            2 => at += 8,
            _ => return None,
        }
    }
    None
}

/// XInput's 32.32 fixed point.
pub(crate) fn fp3232(value: Fp3232) -> f64 {
    f64::from(value.integral) + f64::from(value.frac) / 4_294_967_296.0
}

/// XInput's 16.16 fixed point.
pub(crate) fn fp1616(value: i32) -> f64 {
    f64::from(value) / 65_536.0
}

/// Points to a pixel coordinate the protocol can carry.
pub(crate) fn pixel(points: f64, scale: f64) -> i16 {
    (points * scale).round().clamp(0.0, f64::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scale_comes_from_xft_dpi_in_quarter_steps() {
        assert_eq!(scale_of(""), 1.0);
        assert_eq!(scale_of("Xft.dpi:\t96\n"), 1.0);
        assert_eq!(scale_of("*customization: -color\nXft.dpi:\t192\nXft.antialias: 1\n"), 2.0);
        assert_eq!(scale_of("Xft.dpi: 144"), 1.5);
        // A text size ten percent up is not a scale.
        assert_eq!(scale_of("Xft.dpi: 105.6"), 1.0);
        assert_eq!(scale_of("Xft.dpi: nonsense"), 1.0);
        assert_eq!(scale_of("Xft.dpi: 9600"), 4.0);
    }

    #[test]
    fn the_window_scale_is_read_out_of_xsettings() {
        fn setting(kind: u8, name: &str, value: &[u8]) -> Vec<u8> {
            let mut bytes = vec![kind, 0];
            bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
            bytes.extend_from_slice(name.as_bytes());
            bytes.resize(bytes.len().div_ceil(4) * 4, 0);
            bytes.extend_from_slice(&7u32.to_le_bytes());
            bytes.extend_from_slice(value);
            bytes
        }
        let mut string = 9u32.to_le_bytes().to_vec();
        string.extend_from_slice(b"Adwaita!!\0\0\0");
        let mut data = vec![0, 0, 0, 0];
        data.extend_from_slice(&42u32.to_le_bytes());
        data.extend_from_slice(&4u32.to_le_bytes());
        data.extend(setting(1, "Net/ThemeName", &string));
        data.extend(setting(2, "Some/Colour", &[0; 8]));
        data.extend(setting(0, "Gdk/WindowScalingFactor", &2i32.to_le_bytes()));
        data.extend(setting(0, "Xft/DPI", &196_608i32.to_le_bytes()));
        assert_eq!(xsettings_integer(&data, "Gdk/WindowScalingFactor"), Some(2));
        assert_eq!(xsettings_integer(&data, "Xft/DPI"), Some(196_608));
        assert_eq!(xsettings_integer(&data, "Net/ThemeName"), None);
        assert_eq!(xsettings_integer(&data, "Gdk/Missing"), None);
        // Cut short, or nothing at all: no setting, and no panic.
        assert_eq!(xsettings_integer(&data[..data.len() - 30], "Xft/DPI"), None);
        assert_eq!(xsettings_integer(&[], "Xft/DPI"), None);
        // The same from a big-endian daemon.
        let mut big = vec![1, 0, 0, 0];
        big.extend_from_slice(&1u32.to_be_bytes());
        big.extend_from_slice(&1u32.to_be_bytes());
        big.extend_from_slice(&[0, 0, 0, 3]);
        big.extend_from_slice(b"A/B\0");
        big.extend_from_slice(&1u32.to_be_bytes());
        big.extend_from_slice(&3i32.to_be_bytes());
        assert_eq!(xsettings_integer(&big, "A/B"), Some(3));
    }

    #[test]
    fn fixed_point_values_read_as_numbers() {
        assert_eq!(fp3232(Fp3232 { integral: 3, frac: 1 << 31 }), 3.5);
        assert_eq!(fp3232(Fp3232 { integral: -2, frac: 0 }), -2.0);
        assert_eq!(fp3232(Fp3232 { integral: -1, frac: 3 << 30 }), -0.25);
        assert_eq!(fp1616(1919 << 16), 1919.0);
        assert_eq!(pixel(960.0, 2.0), 1920);
        assert_eq!(pixel(-5.0, 1.0), 0);
    }

    #[test]
    fn the_authority_file_gives_the_cookie_for_the_display() {
        fn entry(family: u16, address: &[u8], number: &[u8], name: &[u8], data: &[u8]) -> Vec<u8> {
            let mut bytes = family.to_be_bytes().to_vec();
            for field in [address, number, name, data] {
                bytes.extend_from_slice(&(field.len() as u16).to_be_bytes());
                bytes.extend_from_slice(field);
            }
            bytes
        }
        let mut file = entry(256, b"desk", b"1", b"MIT-MAGIC-COOKIE-1", b"one");
        file.extend(entry(256, b"desk", b"0", b"XDM-AUTHORIZATION-1", b"other"));
        file.extend(entry(256, b"desk", b"0", b"MIT-MAGIC-COOKIE-1", b"zero"));
        file.extend(entry(65535, b"", b"", b"MIT-MAGIC-COOKIE-1", b"any"));
        file.extend_from_slice(&[0, 1, 0]); // cut short
        let path = std::env::temp_dir().join(format!("rinflow-xauth-test-{}", std::process::id()));
        std::fs::write(&path, &file).unwrap();
        let local = Family::LOCAL;
        assert_eq!(cookie(&path, local, b"desk", 0).unwrap().1, b"zero");
        assert_eq!(cookie(&path, local, b"desk", 1).unwrap().1, b"one");
        // Another host, another display: the entry left open.
        assert_eq!(cookie(&path, local, b"elsewhere", 7).unwrap().1, b"any");
        assert_eq!(cookie(&path, Family::INTERNET, b"desk", 0).unwrap().1, b"any");
        std::fs::remove_file(&path).unwrap();
        assert!(cookie(&path, local, b"desk", 0).is_none());
    }
}
