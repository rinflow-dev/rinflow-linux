//! `rinflowctl`: the agent from a terminal. It speaks the window's
//! protocol (`rinflow_shell::protocol`) over the same socket, so it does
//! what the window does: on a machine without a display, from a script, or
//! bound to a key in the desktop's own keyboard shortcuts
//! (`rinflowctl lock toggle`, `rinflowctl home`).
//!
//! Arranging devices needs no drawing: `place <device> left|right|above|
//! below` attaches a device to a side of this one (what dragging does
//! nearly always), `place <device> --at X,Y` gives the offset in logical
//! points (snapped by the core, as a drop is), and `layout` prints every
//! display with its coordinates to work from.

#[allow(dead_code)]
#[path = "../paths.rs"]
mod paths;

use rinflow_shell::i18n::Text;
use rinflow_shell::protocol::PROTOCOL_VERSION;
use rinflow_shell::state::{BackendView, DeviceView, SessionStatus, Snapshot};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const USAGE: &str = "\
Usage: rinflowctl <command>

  status [--json]                 this device, its devices, input and lock
  devices                         the devices known here, one per line
  layout                          displays and placed devices, with coordinates

  connect <device>                open a session with a device
  disconnect <device>
  forget <device>                 disconnect, unplace and unpair it

  place <device> left|right|above|below
                                  attach the device to that side of this one
  place <device> --at X,Y         put its top-left corner at X,Y (logical
                                  points in this device's space; snapped to
                                  the nearest valid spot)
  unplace <device>

  lock [on|off|toggle]            keep the pointer on the screen it is on
  home                            bring the pointer back from another device
  pair accept|reject              answer the pairing that is waiting
  set <setting> <value>           change a setting (see `status --json`)
  retry-input                     ask the desktop for input permission again
  quit                            stop the agent

<device> is a device's name or ID, or the start of either.";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = arguments.iter().map(String::as_str).collect();
    if matches!(words.first(), None | Some(&"help" | &"--help" | &"-h")) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if words[0] == "--version" {
        println!("rinflowctl {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    match run(&words) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("rinflowctl: {message}");
            ExitCode::FAILURE
        }
    }
}

/// A connection to the agent, with the snapshot it last sent.
struct Agent {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    snapshot: Snapshot,
    /// The last snapshot as it came, for `status --json`.
    raw: Value,
}

enum Incoming {
    State,
    Error(String),
    Other,
}

impl Agent {
    fn connect() -> Result<Self, String> {
        let path = paths::socket_path();
        let stream = UnixStream::connect(&path)
            .map_err(|error| format!("the agent isn't running ({}: {error}). Start it with `rinflow-agent`, or `systemctl --user start rinflow-agent`.", path.display()))?;
        let writer = stream.try_clone().map_err(|error| error.to_string())?;
        let mut agent = Self {
            reader: BufReader::new(stream),
            writer,
            snapshot: empty_snapshot(),
            raw: Value::Null,
        };
        agent.send(json!({"type": "hello", "protocol": PROTOCOL_VERSION}))?;
        // `hello`, then the first snapshot.
        let deadline = Instant::now() + Duration::from_secs(5);
        while agent.raw.is_null() {
            if let Incoming::Error(message) = agent.read(deadline)? {
                return Err(message);
            }
        }
        Ok(agent)
    }

    fn send(&mut self, request: Value) -> Result<(), String> {
        let mut line = request.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).map_err(|error| format!("cannot reach the agent: {error}"))
    }

    /// One message, waited for until `deadline`.
    fn read(&mut self, deadline: Instant) -> Result<Incoming, String> {
        let left = deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
        let _ = self.reader.get_ref().set_read_timeout(Some(left));
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => return Err("the agent closed the connection".into()),
            Ok(_) => {}
            Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                return Err("the agent did not answer in time".into());
            }
            Err(error) => return Err(format!("cannot read from the agent: {error}")),
        }
        let message: Value = serde_json::from_str(&line).map_err(|error| format!("unreadable message from the agent: {error}"))?;
        match message["type"].as_str() {
            Some("hello") => {
                let theirs = message["protocol"].as_u64().unwrap_or(0);
                if theirs != u64::from(PROTOCOL_VERSION) {
                    return Err(format!(
                        "the agent ({}) and rinflowctl are different versions (protocol {theirs} / {PROTOCOL_VERSION}); restart the agent",
                        message["agent_version"].as_str().unwrap_or("?")
                    ));
                }
                Ok(Incoming::Other)
            }
            Some("state") => {
                self.snapshot = serde_json::from_value(message.clone()).map_err(|error| format!("unreadable state from the agent: {error}"))?;
                self.raw = message;
                Ok(Incoming::State)
            }
            Some("error") => {
                let text: Text = serde_json::from_value(message["message"].clone()).unwrap_or_else(|_| Text::from("unknown error"));
                Ok(Incoming::Error(text.to_string()))
            }
            _ => Ok(Incoming::Other),
        }
    }

    /// Sends a request and waits until `done` holds for the snapshot, the
    /// agent refuses, or `seconds` pass (`Err` then).
    fn request(&mut self, request: Value, seconds: f64, done: impl Fn(&Snapshot) -> bool) -> Result<(), String> {
        self.send(request)?;
        let deadline = Instant::now() + Duration::from_secs_f64(seconds);
        loop {
            if done(&self.snapshot) {
                return Ok(());
            }
            match self.read(deadline)? {
                Incoming::Error(message) => return Err(message),
                Incoming::State | Incoming::Other => {}
            }
        }
    }

    /// Sends a request whose effect shows nowhere in particular: waits a
    /// moment for a refusal.
    fn tell(&mut self, request: Value) -> Result<(), String> {
        self.send(request)?;
        let deadline = Instant::now() + Duration::from_millis(400);
        loop {
            match self.read(deadline) {
                Ok(Incoming::Error(message)) => return Err(message),
                Ok(_) => {}
                Err(_) => return Ok(()),
            }
        }
    }

    /// The device the user means: an ID or a name, whole or its start.
    fn device(&self, word: &str) -> Result<Named, String> {
        let mut all: Vec<Named> = self
            .snapshot
            .layout
            .devices
            .iter()
            .map(|device| Named { id: device.peer_id.clone(), name: device.name.clone() })
            .collect();
        for peer in &self.snapshot.peers {
            if !all.iter().any(|known| known.id == peer.id) {
                all.push(Named { id: peer.id.clone(), name: peer.name.clone() });
            }
        }
        let lower = word.to_lowercase();
        let exact: Vec<&Named> = all.iter().filter(|d| d.id == word || d.name.to_lowercase() == lower).collect();
        let matches = if exact.is_empty() {
            all.iter().filter(|d| d.id.starts_with(word) || d.name.to_lowercase().starts_with(&lower)).collect()
        } else {
            exact
        };
        match matches.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(format!("no device called “{word}”; see `rinflowctl devices`")),
            several => Err(format!(
                "“{word}” could be {}; use more of the name, or the ID",
                several.iter().map(|d| format!("{} ({})", d.name, d.id)).collect::<Vec<_>>().join(" or ")
            )),
        }
    }
}

#[derive(Clone)]
struct Named {
    id: String,
    name: String,
}

fn empty_snapshot() -> Snapshot {
    rinflow_shell::state::SharedState::new("", "", Default::default(), false, 0, "").snapshot()
}

fn run(words: &[&str]) -> Result<(), String> {
    let mut agent = Agent::connect()?;
    match words {
        ["status"] => {
            print_status(&agent.snapshot);
            Ok(())
        }
        ["status", "--json"] => {
            println!("{}", serde_json::to_string_pretty(&agent.raw).unwrap_or_default());
            Ok(())
        }
        ["devices"] => {
            print_devices(&agent.snapshot);
            Ok(())
        }
        ["layout"] => {
            print_layout(&agent.snapshot);
            Ok(())
        }
        ["connect", device] => {
            let device = agent.device(device)?;
            let id = device.id.clone();
            agent.request(json!({"type": "connect", "peer_id": device.id}), 15.0, move |s| {
                s.sessions.iter().any(|session| session.peer_id == id && session.status == SessionStatus::Connected)
            })?;
            println!("Connected to {}.", device.name);
            Ok(())
        }
        ["disconnect", device] => {
            let device = agent.device(device)?;
            let id = device.id.clone();
            agent.request(json!({"type": "disconnect", "peer_id": device.id}), 5.0, move |s| {
                !s.sessions.iter().any(|session| session.peer_id == id)
            })
        }
        ["forget", device] => {
            let device = agent.device(device)?;
            agent.tell(json!({"type": "forget_device", "peer_id": device.id}))
        }
        ["place", device, "--at", point] => {
            let device = agent.device(device)?;
            let (x, y) = point
                .split_once(',')
                .and_then(|(x, y)| Some((x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?)))
                .filter(|(x, y)| x.is_finite() && y.is_finite())
                .ok_or("--at takes X,Y in logical points, for example --at 1920,0")?;
            let before = placement(&agent.snapshot, &device.id);
            let id = device.id.clone();
            agent.request(json!({"type": "set_placement", "peer_id": device.id, "x": x, "y": y}), 5.0, move |s| {
                let now = placement(s, &id);
                now.is_some() && (now != before || now == Some((x, y)))
            })?;
            report_placement(&agent.snapshot, &device);
            Ok(())
        }
        ["place", device, side] => {
            let side = match *side {
                "left" => "left",
                "right" => "right",
                "above" | "top" => "top",
                "below" | "bottom" => "bottom",
                other => return Err(format!("“{other}” isn't a side: left, right, above or below (or --at X,Y)")),
            };
            let device = agent.device(device)?;
            let id = device.id.clone();
            agent.request(json!({"type": "place_beside", "peer_id": device.id, "side": side}), 5.0, move |s| {
                s.layout.devices.iter().any(|d| d.peer_id == id && d.offset.is_some() && d.side.is_some_and(|now| side_word(now) == side))
            })?;
            report_placement(&agent.snapshot, &device);
            Ok(())
        }
        ["unplace", device] => {
            let device = agent.device(device)?;
            let id = device.id.clone();
            agent.request(json!({"type": "remove_placement", "peer_id": device.id}), 5.0, move |s| placement(s, &id).is_none())
        }
        ["lock"] | ["lock", "on"] => set_locked(&mut agent, Some(true)),
        ["lock", "off"] | ["unlock"] => set_locked(&mut agent, Some(false)),
        ["lock", "toggle"] => set_locked(&mut agent, None),
        ["home"] => agent.tell(json!({"type": "home"})),
        ["pair", answer @ ("accept" | "reject")] => {
            let Some(pairing) = agent.snapshot.pairing.clone() else {
                return Err("no pairing is waiting".into());
            };
            let kind = if *answer == "accept" { "accept_pairing" } else { "reject_pairing" };
            agent.tell(json!({"type": kind, "peer_id": pairing.peer_id}))?;
            if *answer == "accept" {
                println!("Accepted the pairing with {} (code {}). Accept it there too.", pairing.name, pairing.code);
            }
            Ok(())
        }
        ["set", key, value] => {
            // JSON where it parses (true, 250, "dark"), the word itself otherwise.
            let value: Value = serde_json::from_str(value).unwrap_or_else(|_| Value::from(*value));
            agent.tell(json!({"type": "update_settings", "values": {(*key): value}}))
        }
        ["retry-input"] => agent.tell(json!({"type": "retry_input"})),
        ["quit"] => agent.send(json!({"type": "quit"})),
        _ => Err(format!("unknown command; `rinflowctl help` lists them\n\n{USAGE}")),
    }
}

fn set_locked(agent: &mut Agent, locked: Option<bool>) -> Result<(), String> {
    let want = locked.unwrap_or(!agent.snapshot.locked);
    agent.request(json!({"type": "set_locked", "locked": want}), 3.0, move |s| s.locked == want)?;
    println!("{}", if want { "The pointer stays on this screen." } else { "The pointer can cross again." });
    Ok(())
}

fn placement(snapshot: &Snapshot, peer_id: &str) -> Option<(f64, f64)> {
    snapshot
        .layout
        .devices
        .iter()
        .find(|device| device.peer_id == peer_id)
        .and_then(|device| device.offset)
        .map(|offset| (offset.x, offset.y))
}

fn side_word(side: rinflow_core::layout::Side) -> &'static str {
    use rinflow_core::layout::Side;
    match side {
        Side::Left => "left",
        Side::Right => "right",
        Side::Top => "top",
        Side::Bottom => "bottom",
    }
}

fn side_phrase(side: rinflow_core::layout::Side) -> &'static str {
    use rinflow_core::layout::Side;
    match side {
        Side::Left => "on the left",
        Side::Right => "on the right",
        Side::Top => "above",
        Side::Bottom => "below",
    }
}

fn report_placement(snapshot: &Snapshot, device: &Named) {
    if let Some(placed) = snapshot.layout.devices.iter().find(|d| d.peer_id == device.id)
        && let Some(offset) = placed.offset
    {
        let side = placed.side.map(|side| format!(" ({})", side_phrase(side))).unwrap_or_default();
        println!("{} is at {},{}{side}.", device.name, number(offset.x), number(offset.y));
    }
}

/// Whole numbers without a fraction: coordinates are nearly always whole.
fn number(value: f64) -> String {
    if value.fract() == 0.0 { format!("{value:.0}") } else { format!("{value:.1}") }
}

fn backend(view: &BackendView) -> String {
    let state = serde_json::to_value(view.state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    let detail = view.detail.to_string();
    if detail.is_empty() { state } else { format!("{state}: {detail}") }
}

fn device_line(snapshot: &Snapshot, device: &DeviceView) -> String {
    let session = snapshot.sessions.iter().find(|s| s.peer_id == device.peer_id);
    let online = snapshot.peers.iter().any(|p| p.id == device.peer_id);
    let mut notes = vec![match session {
        Some(s) if s.status == SessionStatus::Connected => match s.latency_ms {
            Some(ms) => format!("connected, {ms} ms"),
            None => "connected".to_owned(),
        },
        Some(_) => "connecting".to_owned(),
        None if online => "online".to_owned(),
        None => "offline".to_owned(),
    }];
    if snapshot.driving.as_deref() == Some(device.peer_id.as_str()) {
        notes.push("you are controlling it".into());
    }
    if session.is_some_and(|s| s.remote_control) {
        notes.push("controlling this device".into());
    }
    notes.push(match (device.side, device.offset) {
        (Some(side), _) => side_phrase(side).to_owned(),
        (None, Some(_)) => "placed".to_owned(),
        (None, None) => "not placed".to_owned(),
    });
    if !snapshot.paired_peers.contains(&device.peer_id) {
        notes.push("not paired".into());
    }
    format!("{}  [{}]  {}", device.name, device.peer_id, notes.join(", "))
}

fn print_devices(snapshot: &Snapshot) {
    for device in &snapshot.layout.devices {
        println!("{}", device_line(snapshot, device));
    }
    for peer in snapshot.peers.iter().filter(|p| !snapshot.layout.devices.iter().any(|d| d.peer_id == p.id)) {
        println!("{}  [{}]  nearby, {}", peer.name, peer.id, peer.address);
    }
}

fn print_status(snapshot: &Snapshot) {
    println!("{}  [{}]  agent {}", snapshot.device_name, snapshot.device_id, snapshot.agent_version);
    println!("  controlling others:   {}", backend(&snapshot.input.capture));
    println!("  being controlled:     {}", backend(&snapshot.input.emulation));
    println!("  sharing:              {}", if snapshot.settings.sharing { "on" } else { "paused" });
    println!("  pointer:              {}", if snapshot.locked { "locked to its screen" } else { "free to cross" });
    if let Some(error) = &snapshot.last_error {
        println!("  problem:              {error}");
    }
    if let Some(transfer) = &snapshot.transfer {
        println!("  receiving files:      {} ({}%)", transfer.name, transfer.percent());
    }
    if let Some(pairing) = &snapshot.pairing {
        println!("  pairing:              {} asks to pair, code {} (rinflowctl pair accept|reject)", pairing.name, pairing.code);
    }
    println!();
    if snapshot.layout.devices.is_empty() && snapshot.peers.is_empty() {
        println!("No devices yet.");
    } else {
        print_devices(snapshot);
    }
}

fn print_layout(snapshot: &Snapshot) {
    println!("Logical points; x grows to the right, y downward. This device:");
    for (index, display) in snapshot.layout.displays.iter().enumerate() {
        println!(
            "  display {}   {}×{} at {},{}",
            index + 1,
            number(display.width),
            number(display.height),
            number(display.x),
            number(display.y)
        );
    }
    for device in &snapshot.layout.devices {
        let size = device
            .displays
            .iter()
            .map(|d| format!("{}×{}", number(d.width), number(d.height)))
            .collect::<Vec<_>>()
            .join(" + ");
        match device.offset {
            Some(offset) => println!(
                "{}: {size} at {},{}{}",
                device.name,
                number(offset.x),
                number(offset.y),
                device.side.map(|side| format!(" ({})", side_phrase(side))).unwrap_or_default()
            ),
            None => println!("{}: {size}, not placed", device.name),
        }
    }
}
