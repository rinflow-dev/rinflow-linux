//! Moves the pointer and presses keys on the local desktop through the
//! agent's own emulation backend (RemoteDesktop portal + libei), as a test's
//! stand-in for a physical mouse. Commands over a control socket, one JSON
//! object per line: {"abs":[x,y]}, {"rel":[dx,dy]}, {"key":[code,down]},
//! {"button":[n,down]}, {"state":true} (answers {"ready":bool}).
//!
//!   injector <control socket> [--socket <EIS socket>]   (gamescope instead of the portal)

use rinflow_core::{
    LC_INPUT_FLAG_ABSOLUTE_POSITION, LC_INPUT_KEY_DOWN, LC_INPUT_KEY_UP,
    LC_INPUT_MOUSE_BUTTON_DOWN, LC_INPUT_MOUSE_BUTTON_UP, LC_INPUT_MOUSE_MOVE, LcInputEvent,
};
use rinflow_input::{EisSource, EmulationEvent};
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let control = arguments.first().cloned().expect("injector <control socket path> [--socket <eis socket>]");
    let source = match arguments.iter().position(|a| a == "--socket") {
        Some(i) => EisSource::Socket(PathBuf::from(arguments.get(i + 1).expect("--socket <path>"))),
        None => EisSource::Portal,
    };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async move {
        let state_dir = PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or("/tmp".into())).join("injector-state");
        let (input_tx, input_rx) = mpsc::unbounded_channel::<LcInputEvent>();
        let (events_tx, mut events_rx) = mpsc::unbounded_channel::<EmulationEvent>();
        let (_shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        tokio::task::spawn_local(async move {
            rinflow_input::ei_emulation::run(source, &state_dir, input_rx, events_tx, shutdown_rx, std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0))).await;
            log::info!("emulation backend ended");
        });
        let ready = Rc::new(Cell::new(false));
        let ready_flag = ready.clone();
        tokio::task::spawn_local(async move {
            while let Some(event) = events_rx.recv().await {
                log::info!("emulation: {event:?}");
                ready_flag.set(event == EmulationEvent::Ready);
            }
        });
        let _ = std::fs::remove_file(&control);
        let listener = tokio::net::UnixListener::bind(&control).expect("control socket");
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(command) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
                let pair = |key: &str| {
                    command[key].as_array().map(|a| (a[0].as_f64().unwrap_or(0.0), a[1].clone()))
                };
                let reply = if command.get("state").is_some() {
                    serde_json::json!({"ready": ready.get()})
                } else {
                    let event = if let Some((x, y)) = pair("abs") {
                        LcInputEvent { kind: LC_INPUT_MOUSE_MOVE, flags: LC_INPUT_FLAG_ABSOLUTE_POSITION, x, y: y.as_f64().unwrap_or(0.0), ..Default::default() }
                    } else if let Some((dx, dy)) = pair("rel") {
                        LcInputEvent { kind: LC_INPUT_MOUSE_MOVE, delta_x: dx, delta_y: dy.as_f64().unwrap_or(0.0), ..Default::default() }
                    } else if let Some((code, down)) = pair("key") {
                        LcInputEvent { kind: if down.as_bool().unwrap_or(true) { LC_INPUT_KEY_DOWN } else { LC_INPUT_KEY_UP }, code: code as u32, ..Default::default() }
                    } else if let Some((button, down)) = pair("button") {
                        LcInputEvent { kind: if down.as_bool().unwrap_or(true) { LC_INPUT_MOUSE_BUTTON_DOWN } else { LC_INPUT_MOUSE_BUTTON_UP }, code: button as u32, ..Default::default() }
                    } else {
                        let _ = writer.write_all(b"{\"ok\":false}\n").await;
                        continue;
                    };
                    serde_json::json!({"ok": input_tx.send(event).is_ok()})
                };
                let mut text = reply.to_string();
                text.push('\n');
                if writer.write_all(text.as_bytes()).await.is_err() {
                    break;
                }
            }
        }
    });
}
