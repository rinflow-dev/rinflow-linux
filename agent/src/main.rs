//! Rinflow's Linux agent. Runs for as long as the user is signed in,
//! whether or not a desktop is up: the core keeps devices connected, the
//! input backends come and go with the compositor (see `backends.rs`), and
//! the window talks to it over a socket (see `ipc.rs`).

mod autostart;
mod backends;
mod displays;
mod driver;
mod ipc;
mod paths;
mod power;
mod services;

use backends::Backends;
use driver::Driver;
use rinflow_core::{Core, CoreConfig, LcInputEvent};
use rinflow_shell::arrangement::Arrangement;
use rinflow_shell::i18n;
use rinflow_shell::identity;
use rinflow_shell::settings::SettingsStore;
use rinflow_shell::state::SharedState;
use rinflow_shell::trust::TrustStore;
use rinflow_input::{CaptureEvent, EmulationEvent};
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
    let listener = match ipc::listen(&paths::socket_path()).await {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            log::info!("another agent is already running");
            return 0;
        }
        Err(error) => {
            log::error!("cannot listen on {}: {error}", paths::socket_path().display());
            return 3;
        }
    };

    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
        .unwrap_or_default();
    i18n::set_system_chinese(i18n::locale_is_simplified_chinese(&locale));

    let settings = Arc::new(SettingsStore::load(&data));
    let preferences = settings.get();
    i18n::set_language(&preferences.language);
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
    });
    tokio::task::spawn_local(ipc::serve(listener, services));
    power::spawn(Arc::downgrade(&core));

    let (capture_events_tx, mut capture_events) = mpsc::unbounded_channel::<CaptureEvent>();
    let (emulation_events_tx, mut emulation_events) = mpsc::unbounded_channel::<EmulationEvent>();
    let mut backends = Backends::new(state.clone(), state_dir.clone(), capture_events_tx, emulation_events_tx);
    let mut driver = Driver::new(Arc::downgrade(&core), state.clone(), arrangement.clone(), settings.clone(), backends.last_played.clone());

    let mut poll = tokio::time::interval(Duration::from_secs(1));
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    log::info!("rinflow-agent {} as {} ({})", env!("CARGO_PKG_VERSION"), identity.device_name, identity.device_id);

    loop {
        tokio::select! {
            _ = poll.tick() => {
                if backends.poll() {
                    driver.set_capture(backends.capture_commands());
                }
            }
            _ = tick.tick(), if driver.driving() => driver.tick(),
            json = core_events.recv() => {
                let Some(json) = json else { break };
                let Ok(event) = serde_json::from_str::<serde_json::Value>(&json) else { continue };
                driver.core_event(&event);
                if matches!(
                    event["event"].as_str(),
                    Some("placement_changed" | "peer_displays" | "peer_up" | "remote_control_started" | "remote_control_ended" | "disconnected")
                ) {
                    driver.refresh_barriers();
                }
            }
            event = input.recv() => {
                let Some(event) = event else { break };
                backends.play(event);
            }
            event = capture_events.recv() => {
                let Some(event) = event else { break };
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
                let Some(event) = event else { break };
                backends.on_emulation_event(&event);
            }
            command = commands.recv() => match command {
                Some(AgentCommand::SettingsChanged(keys)) => {
                    if keys.iter().any(|key| key == "sharing") {
                        driver.refresh_barriers();
                    }
                }
                Some(AgentCommand::RetryInput) => {
                    driver.set_capture(None);
                    backends.retry();
                }
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
