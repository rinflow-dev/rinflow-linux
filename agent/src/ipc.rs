//! The window's socket: JSON lines over a Unix socket in the runtime
//! directory (see `rinflow_shell::protocol`). Every client gets `hello`,
//! the current snapshot, and then every snapshot after a change.

use crate::services::{AgentCommand, Services};
use rinflow_core::layout::Offset;
use rinflow_shell::i18n::Text;
use rinflow_shell::protocol::{Message, PROTOCOL_VERSION, Request};
use std::io;
use std::path::Path;
use std::rc::Rc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Binds the socket. If another agent answers on it, this one must not
/// start: `Err(AlreadyExists)`.
pub async fn listen(path: &Path) -> io::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        set_private(parent);
    }
    if UnixStream::connect(path).await.is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "another agent is running",
        ));
    }
    // A socket file left by an agent that did not exit cleanly.
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    set_private(path);
    Ok(listener)
}

fn set_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

pub async fn serve(listener: UnixListener, services: Rc<Services>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::task::spawn_local(serve_client(stream, services.clone()));
            }
            Err(error) => {
                log::warn!("IPC accept failed: {error}");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
}

async fn serve_client(stream: UnixStream, services: Rc<Services>) {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut snapshots = services.state.subscribe();

    let hello = Message::Hello {
        protocol: PROTOCOL_VERSION,
        agent_version: env!("CARGO_PKG_VERSION"),
    };
    if writer.write_all(hello.to_line().as_bytes()).await.is_err() {
        return;
    }
    let snapshot = snapshots.borrow_and_update().clone();
    if writer
        .write_all(Message::State(&snapshot).to_line().as_bytes())
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { break };
                log::debug!("ipc request: {line}");
                let reply = match handle_request(&line, &services) {
                    Ok(reply) => reply,
                    Err(message) => Some(Message::Error { message }),
                };
                if let Some(reply) = reply
                    && writer.write_all(reply.to_line().as_bytes()).await.is_err()
                {
                    break;
                }
            }
            changed = snapshots.changed() => {
                if changed.is_err() {
                    break;
                }
                let snapshot = snapshots.borrow_and_update().clone();
                log::debug!("ipc push: state");
                if writer.write_all(Message::State(&snapshot).to_line().as_bytes()).await.is_err() {
                    break;
                }
            }
        }
    }
}

/// Acts on one request; some are answered with a message of their own.
fn handle_request(line: &str, services: &Services) -> Result<Option<Message<'static>>, Text> {
    let request = Request::parse(line)?;
    let done = match request {
        Request::PreviewPlacement { peer_id, x, y } => {
            let offset = services.preview_placement(&peer_id, Offset { x, y });
            return Ok(Some(Message::PlacementPreview { peer_id, offset }));
        }
        Request::Hello { protocol } if protocol == PROTOCOL_VERSION => Ok(()),
        Request::Hello { protocol } => Err(Text::from(format!(
            "the window speaks protocol {protocol}, the agent speaks {PROTOCOL_VERSION}"
        ))),
        Request::Connect { peer_id } => Ok(services.core()?.connect(&peer_id)?),
        Request::Disconnect { peer_id } => Ok(services.core()?.disconnect_peer(&peer_id)?),
        Request::SetPlacement { peer_id, x, y } => {
            Ok(services.core()?.set_placement(&peer_id, Some(Offset { x, y }))?)
        }
        Request::RemovePlacement { peer_id } => Ok(services.core()?.set_placement(&peer_id, None)?),
        Request::PlaceBeside { peer_id, side } => Ok(services.core()?.place_beside(&peer_id, side)?),
        Request::UpdateSettings { values } => services.update_settings(values),
        Request::ForgetDevice { peer_id } => services.forget_device(&peer_id),
        Request::AcceptPairing { peer_id } => services.accept_pairing(&peer_id),
        Request::RejectPairing { peer_id } => services.reject_pairing(&peer_id),
        Request::ClearError => {
            services.state.clear_error();
            Ok(())
        }
        Request::RetryInput => {
            let _ = services.commands.send(AgentCommand::RetryInput);
            Ok(())
        }
        Request::Quit => {
            let _ = services.commands.send(AgentCommand::Quit);
            Ok(())
        }
    };
    done.map(|()| None)
}
