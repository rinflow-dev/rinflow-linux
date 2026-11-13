//! Rinflow's Linux agent. Runs for as long as the user is signed in,
//! whether or not a desktop is up: the core keeps devices connected, the
//! input backends come and go with the compositor (see `backends.rs`), and
//! the window talks to it over a socket (see `ipc.rs`).

mod autostart;
mod backends;
mod desktop;
mod displays;
mod driver;
mod ipc;
mod notify;
mod paths;
mod power;
mod remote_files;
mod services;
mod tray;

use backends::Backends;
use driver::Driver;
use rinflow_core::{Core, CoreConfig, LcInputEvent};
use rinflow_shell::arrangement::Arrangement;
use rinflow_shell::i18n::{Text, msg};
use rinflow_shell::identity;
use rinflow_shell::settings::SettingsStore;
use rinflow_shell::state::SharedState;
use rinflow_shell::trust::TrustStore;
use rinflow_input::{CaptureEvent, ClipboardEvent, EmulationEvent};
use services::{AgentCommand, Services};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("--version") => {
            println!("rinflow-agent {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("--launch-at-login") => {
            let on = arguments.get(1).is_some_and(|value| value == "on");
            std::process::exit(i32::from(autostart::set(on).is_err()));
        }
        Some(other) => {
            eprintln!("rinflow-agent: unknown argument {other}");
            std::process::exit(2);
        }
        None => {}
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let local = tokio::task::LocalSet::new();
    let code = local.block_on(&runtime, run());
    std::process::exit(code);
}

async fn run() -> i32 {
    let data = paths::data_directory();
    let state_dir = paths::state_directory();
    let identity = match identity::load(&data, paths::host_name()) {
        Ok(identity) => identity,
        Err(error) => {
            log::error!("cannot load device identity: {error}");
            return 2;
        }
    };
    // `_lock` (not `_`) keeps the single-instance lock until the agent exits.
    let (listener, _lock) = match ipc::listen(&paths::socket_path()).await {
        Ok(listening) => listening,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            log::info!("another agent is already running");
            return 0;
        }
        Err(error) => {
            log::error!("cannot listen on {}: {error}", paths::socket_path().display());
            return 3;
        }
    };

    let settings = Arc::new(SettingsStore::load(&data));
    let preferences = settings.get();
    let control_port = preferences
        .port
        .unwrap_or_else(|| CoreConfig::default().control_port);
    let state = SharedState::new(
        &identity.device_id,
        &identity.device_name,
        preferences.clone(),
        autostart::enabled(),
        control_port,
        env!("CARGO_PKG_VERSION"),
    );
    let display_cache = displays::DisplayCache::new(&state_dir);
    let arrangement = Arrangement::load(&data, state.clone(), display_cache.load());
    let trust = Arc::new(TrustStore::load(&data));
    state.set_paired_peers(trust.peer_ids());

    let (core_events_tx, mut core_events) = mpsc::unbounded_channel::<String>();
    let (input_tx, mut input) = mpsc::unbounded_channel::<LcInputEvent>();
    let core = {
        let state = state.clone();
        let arrangement = arrangement.clone();
        let trust = trust.clone();
        let config = CoreConfig {
            device_id: identity.device_id.clone(),
            device_name: identity.device_name.clone(),
            platform: "linux".to_owned(),
            control_port,
            modifier_map: preferences.modifier_map,
            displays: arrangement.displays(),
            placements: arrangement.placements(),
            // Always offered, so the setting can change at run time; while
            // it is off the agent reports an empty clipboard (see below).
            clipboard: true,
            // Files a peer copied are only described to this device
            // (`files_available`), and read when the user pastes them: see
            // `remote_files.rs`. Nothing is ever downloaded by itself; the
            // directory is one the core asks for and never gets to use.
            files: true,
            on_demand_files: true,
            download_directory: Some(paths::runtime_directory().join("declined").to_string_lossy().into_owned()),
            max_auto_receive_bytes: 0,
            identity_path: Some(identity::identity_path(&data).to_string_lossy().into_owned()),
            trusted_peers: trust.peers(),
            require_pairing: preferences.paired_only,
            manual_addresses: preferences.manual_addresses.clone(),
            ..CoreConfig::default()
        };
        match Core::start(
            config,
            move |json| {
                state.apply_core_event(&json);
                arrangement.core_event(&json);
                if trust.core_event(&json) {
                    state.set_paired_peers(trust.peer_ids());
                }
                let _ = core_events_tx.send(json);
            },
            move |event| {
                let _ = input_tx.send(event);
            },
        ) {
            Ok(core) => Arc::new(core),
            Err(error) => {
                log::error!("cannot start the core: {error}");
                return 4;
            }
        }
    };
    let _ = core.set_remote_control_allowed(preferences.sharing);

    let (commands_tx, mut commands) = mpsc::unbounded_channel::<AgentCommand>();
    let services = Rc::new(Services {
        core: Arc::downgrade(&core),
        state: state.clone(),
        settings: settings.clone(),
        arrangement: arrangement.clone(),
        commands: commands_tx,
        windows: tokio::sync::watch::Sender::new(0),
    });
    tokio::task::spawn_local(ipc::serve(listener, services.clone()));
    // The tray's labels are the only text the agent shows by itself.
    let environment = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let messages_locale = environment("LC_ALL").or_else(|| environment("LC_MESSAGES")).or_else(|| environment("LANG"));
    rinflow_shell::i18n::init(
        rinflow_shell::i18n::locale_directory(),
        rinflow_shell::i18n::system_languages(messages_locale.as_deref(), environment("LANGUAGE").as_deref()),
    );
    tray::spawn(services.clone());
    notify::spawn(services);
    power::spawn(Arc::downgrade(&core));

    let (capture_events_tx, mut capture_events) = mpsc::unbounded_channel::<CaptureEvent>();
    let (emulation_events_tx, mut emulation_events) = mpsc::unbounded_channel::<EmulationEvent>();
    let (clipboard_events_tx, mut clipboard_events) = mpsc::unbounded_channel::<ClipboardEvent>();
    let mut backends = Backends::new(
        state.clone(),
        state_dir.clone(),
        capture_events_tx,
        emulation_events_tx,
        clipboard_events_tx,
    );
    backends.set_clipboard_enabled(preferences.clipboard);
    backends.set_files_enabled(preferences.clipboard);
    let mut driver = Driver::new(Arc::downgrade(&core), state.clone(), arrangement.clone(), settings.clone(), backends.last_played.clone());

    let mut poll = tokio::time::interval(Duration::from_secs(1));
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    // The tick is only polled while driving; the default (Burst) would fire
    // every tick missed while idle back to back at the next crossing.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    log::info!("rinflow-agent {} as {} ({})", env!("CARGO_PKG_VERSION"), identity.device_name, identity.device_id);

    let mut remote_files = remote_files::RemoteFiles::new(paths::runtime_directory().join("files"), Arc::downgrade(&core), {
        let state = state.clone();
        Box::new(move |peer, received| state.set_files_read(peer, received))
    });
    // A peer too old to offer files on demand is told about once.
    let mut told_outdated = false;

    // A core that could not start (its port is taken, say) leaves the agent
    // up: the window shows why, and the port can be changed from there.
    let mut core_running = true;
    loop {
        tokio::select! {
            _ = poll.tick() => {
                if backends.poll().await {
                    driver.set_capture(backends.capture_commands());
                }
            }
            _ = tick.tick(), if driver.ticking() => driver.tick(),
            json = core_events.recv(), if core_running => {
                let Some(json) = json else {
                    core_running = false;
                    let detail = state.snapshot().last_error.map(|error| error.to_string()).unwrap_or_default();
                    log::error!("the core stopped: {detail}");
                    state.set_error(core_stopped_message(control_port, &detail));
                    continue;
                };
                let Ok(event) = serde_json::from_str::<serde_json::Value>(&json) else { continue };
                log_core_event(&event);
                driver.core_event(&event);
                if event["event"] == "clipboard" {
                    received_clipboard(&event, &settings, &core, &backends);
                }
                match event["event"].as_str() {
                    Some("files_available") => files_available(&event, &settings, &core, &backends, &mut remote_files),
                    Some("files_unavailable") => {
                        if let Some(peer) = event["peer_id"].as_str()
                            && remote_files.withdraw(peer)
                        {
                            backends.withdraw_files();
                        }
                    }
                    Some("files_offered") => files_offered(&event, &core, &state, &mut told_outdated),
                    _ => {}
                }
                if matches!(
                    event["event"].as_str(),
                    Some("placement_changed" | "peer_displays" | "peer_up" | "remote_control_started" | "remote_control_ended" | "disconnected")
                ) {
                    driver.refresh_barriers();
                }
            }
            event = input.recv(), if core_running => {
                let Some(event) = event else { continue };
                backends.play(event);
            }
            event = capture_events.recv() => {
                let Some(event) = event else { log::error!("capture events ended"); break };
                backends.on_capture_event(&event);
                match &event {
                    CaptureEvent::Zones(displays) => {
                        display_cache.save(displays);
                        arrangement.set_displays(displays.clone());
                        let _ = core.set_local_displays(displays.clone());
                        driver.refresh_barriers();
                    }
                    CaptureEvent::Ready => driver.set_capture(backends.capture_commands()),
                    CaptureEvent::Lost(_) | CaptureEvent::Denied(_) => driver.set_capture(None),
                    CaptureEvent::Deactivated => {
                        if driver.take_replay() {
                            backends.replay_entry();
                        }
                    }
                    _ => {}
                }
                driver.handle_capture(event);
            }
            event = emulation_events.recv() => {
                let Some(event) = event else { log::error!("emulation events ended"); break };
                backends.on_emulation_event(&event);
            }
            event = clipboard_events.recv() => match event {
                Some(ClipboardEvent::Available(available)) => state.set_clipboard_available(available),
                Some(ClipboardEvent::Changed(clipboard)) => {
                    // While sharing is off peers get an empty clipboard, so
                    // nothing copied here leaves this device.
                    let shared = if settings.get().clipboard { clipboard } else { Default::default() };
                    log::debug!("local clipboard: {} formats", shared.formats.len());
                    let _ = core.set_clipboard(shared);
                }
                None => {
                    log::error!("clipboard events ended");
                    break;
                }
            },
            command = commands.recv() => match command {
                Some(AgentCommand::SettingsChanged(keys)) => {
                    if keys.iter().any(|key| key == "sharing") {
                        driver.refresh_barriers();
                    }
                    if keys.iter().any(|key| key == "clipboard") {
                        let enabled = settings.get().clipboard;
                        // Copied files travel with the clipboard.
                        backends.set_files_enabled(enabled);
                        backends.set_clipboard_enabled(enabled);
                        if !enabled {
                            // Files being read from a peer stop as well.
                            let _ = core.revoke_file_streams();
                            remote_files.withdraw_all();
                            backends.withdraw_files();
                            let _ = core.set_clipboard(Default::default());
                        }
                    }
                }
                Some(AgentCommand::RetryInput) => {
                    driver.set_capture(None);
                    backends.retry();
                }
                Some(AgentCommand::LockChanged) => driver.refresh_barriers(),
                Some(AgentCommand::Home) => driver.home(),
                Some(AgentCommand::Quit) | None => {
                    log::info!("quitting");
                    state.set_quitting();
                    break;
                }
            },
            _ = tokio::signal::ctrl_c() => {
                log::info!("interrupted");
                break;
            }
            _ = async { match terminate.as_mut() { Some(signal) => { signal.recv().await; } None => std::future::pending::<()>().await } } => {
                log::info!("terminated");
                break;
            }
        }
    }
    backends.shutdown();
    // Peers hear the sessions end in order rather than time out.
    let _ = core.disconnect();
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(core);
    let _ = std::fs::remove_file(paths::socket_path());
    0
}

/// Sessions and pairing in the log, to tell afterwards why a device didn't
/// connect or pair: which device, and the reason the core gives. Nothing
/// else of the event (no codes, no clipboard).
fn log_core_event(event: &serde_json::Value) {
    let Some(kind @ ("connected" | "disconnected" | "pairing_requested" | "paired" | "pairing_ended" | "unpaired" | "error")) =
        event["event"].as_str()
    else {
        return;
    };
    let field = |key: &str| event[key].as_str().unwrap_or_default();
    let peer = [field("peer_id"), field("peer")].into_iter().find(|id| !id.is_empty()).unwrap_or("-");
    let detail = [field("reason"), field("message")].into_iter().find(|text| !text.is_empty()).unwrap_or("");
    if kind == "error" || (kind == "disconnected" && !detail.is_empty()) {
        log::warn!("core {kind}: {peer} {detail}");
    } else {
        log::info!("core {kind}: {peer} {detail}");
    }
}

/// What the window says when the core stopped (it only does at start-up).
fn core_stopped_message(port: u16, detail: &str) -> Text {
    if detail.starts_with("control port failed") || detail.starts_with("discovery socket failed") {
        // Translators: {detail} is the system's error message, in English.
        msg("Rinflow couldn't use UDP port {port}; another copy of Rinflow may be running. Choose another port in Settings, then restart the agent. ({detail})")
            .arg("port", port.to_string())
            .arg("detail", detail)
    } else {
        msg("Rinflow's core didn't start: {detail}").arg("detail", detail)
    }
}

/// A peer describes the files copied there. They appear in this device's
/// file system without their contents (`remote_files.rs`) and go on the
/// clipboard as files; what is pasted is read from the peer then.
fn files_available(
    event: &serde_json::Value,
    settings: &SettingsStore,
    core: &Core,
    backends: &Backends,
    remote_files: &mut remote_files::RemoteFiles,
) {
    let Some(peer) = event["peer_id"].as_str() else { return };
    if !settings.get().clipboard {
        let _ = core.set_clipboard(Default::default());
        return;
    }
    let manifest = match serde_json::from_value::<rinflow_core::FileManifest>(event["manifest"].clone()) {
        Ok(manifest) => manifest,
        Err(error) => return log::warn!("unreadable files from a peer: {error}"),
    };
    let Some(paths) = remote_files.publish(peer, &manifest) else { return };
    log::info!("files from {peer}: {} offered, {} bytes, read when pasted", manifest.entries.len(), manifest.total_size);
    backends.offer_clipboard(rinflow_core::Clipboard {
        formats: vec![rinflow_core::ClipboardFormat {
            mime: rinflow_core::MIME_URI_LIST.to_owned(),
            data: remote_files::uri_list(&paths).into_bytes(),
        }],
    });
}

/// A peer without file streams offers files the old way, which would
/// download them here at once: declined, and the window says why once.
fn files_offered(event: &serde_json::Value, core: &Core, state: &SharedState, told: &mut bool) {
    let Some(transfer) = event["transfer"].as_u64().and_then(|transfer| u32::try_from(transfer).ok()) else {
        return;
    };
    let _ = core.cancel_files(transfer);
    let peer = event["peer_id"].as_str().unwrap_or_default();
    log::info!("files from {peer}: declined (it has no file streams)");
    if !std::mem::replace(told, true) {
        let name = state.snapshot().sessions.iter().find(|session| session.peer_id == peer).map(|session| session.name.clone());
        // Translators: {name} is another device's name.
        state.set_error(
            msg("Files copied on {name} can't be pasted here: update Rinflow on that device.").arg("name", name.unwrap_or_else(|| peer.to_owned())),
        );
    }
}

/// A peer's clipboard arrived (control came back from it, or it started
/// driving this device): this desktop's applications can paste it. With
/// sharing off it is dropped, and the core's copy is cleared so it does not
/// travel on to the next peer.
fn received_clipboard(event: &serde_json::Value, settings: &SettingsStore, core: &Core, backends: &Backends) {
    if !settings.get().clipboard {
        let _ = core.set_clipboard(Default::default());
        return;
    }
    match rinflow_core::Clipboard::from_json(event) {
        Ok(clipboard) => {
            log::info!(
                "clipboard from {}: {:?}",
                event["peer_id"].as_str().unwrap_or("?"),
                clipboard.formats.iter().map(|f| (f.mime.as_str(), f.data.len())).collect::<Vec<_>>()
            );
            backends.offer_clipboard(clipboard);
        }
        Err(error) => log::warn!("unreadable clipboard from a peer: {error}"),
    }
}
