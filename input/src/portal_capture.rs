//! Capture through `org.freedesktop.portal.InputCapture` and libei.
//!
//! One session for the life of the backend: barrier changes go through
//! `SetPointerBarriers` + `Enable` on the same session, and a `Disabled`
//! signal is answered by enabling again. Recreating sessions is what runs
//! compositors out of file descriptors (lan-mouse #472, xdph #419).

use crate::ei_common::handshake;
use crate::keymap::{button_number, hid_usage};
use crate::token::TokenFile;
use crate::{Barrier, CaptureCommand, CaptureEvent};
use ashpd::desktop::input_capture::{
    Activated, ActivatedBarrier, Barrier as PortalBarrier, Capabilities, CreateSession2Options,
    CreateSessionOptions, InputCapture, StartOptions, Zones,
};
use ashpd::desktop::{PersistMode, Session};
use futures_util::StreamExt;
use rinflow_core::layout::Display;
use reis::ei;
use reis::event::{DeviceCapability, EiEvent};
use std::num::NonZeroU32;
use std::path::Path;
use tokio::sync::mpsc;

/// Points per notch of a wheel without high-resolution scrolling: the
/// core's convention.
const POINTS_PER_NOTCH: f64 = 10.0;

/// Runs until `Shutdown`, or until the session is lost or denied (reported
/// as the last event). `state_directory` keeps the restore token.
pub async fn run(
    state_directory: &Path,
    mut commands: mpsc::UnboundedReceiver<CaptureCommand>,
    events: mpsc::UnboundedSender<CaptureEvent>,
) {
    let report = |event: CaptureEvent| {
        let _ = events.send(event);
    };
    let token = TokenFile::new(state_directory, "input-capture");
    let portal = match InputCapture::new().await {
        Ok(portal) => portal,
        Err(error) => {
            report(CaptureEvent::Lost(format!("InputCapture portal: {error}")));
            return;
        }
    };
    let session = match open_session(&portal, &token, &report).await {
        Ok(session) => session,
        Err(event) => {
            report(event);
            return;
        }
    };
    let fd = match portal.connect_to_eis(&session, Default::default()).await {
        Ok(fd) => fd,
        Err(error) => {
            report(CaptureEvent::Lost(format!("ConnectToEIS: {error}")));
            let _ = session.close().await;
            return;
        }
    };
    let (context, _connection, mut ei_events) =
        match handshake(fd, ei::handshake::ContextType::Receiver).await {
            Ok(parts) => parts,
            Err(error) => {
                report(CaptureEvent::Lost(error));
                let _ = session.close().await;
                return;
            }
        };
    let (mut activated, mut deactivated, mut disabled, mut zones_changed) = match (
        portal.receive_activated().await,
        portal.receive_deactivated().await,
        portal.receive_disabled().await,
        portal.receive_zones_changed().await,
    ) {
        (Ok(a), Ok(d), Ok(x), Ok(z)) => (a, d, x, z),
        _ => {
            report(CaptureEvent::Lost("cannot listen to portal signals".into()));
            let _ = session.close().await;
            return;
        }
    };

    let mut zones = match fetch_zones(&portal, &session).await {
        Ok(zones) => zones,
        Err(error) => {
            report(CaptureEvent::Lost(format!("GetZones: {error}")));
            let _ = session.close().await;
            return;
        }
    };
    report(CaptureEvent::Zones(displays_of(&zones)));
    report(CaptureEvent::Ready);

    let mut barriers: Vec<Barrier> = Vec::new();
    let mut activation: Option<Option<u32>> = None;

    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(CaptureCommand::SetBarriers(new)) => {
                    barriers = new;
                    if let Err(error) = apply_barriers(&portal, &session, &zones, &barriers).await {
                        report(CaptureEvent::Lost(format!("SetPointerBarriers: {error}")));
                        break;
                    }
                }
                Some(CaptureCommand::Release { x, y }) => {
                    let Some(id) = activation.take() else { continue };
                    let options = ashpd::desktop::input_capture::ReleaseOptions::default()
                        .set_activation_id(id)
                        .set_cursor_position(Some((x, y)));
                    if let Err(error) = portal.release(&session, options).await {
                        log::warn!("Release: {error}");
                    }
                }
                Some(CaptureCommand::Shutdown) | None => break,
            },
            signal = activated.next() => {
                let Some(signal) = signal else {
                    report(CaptureEvent::Lost("Activated signal stream ended".into()));
                    break;
                };
                let Some((barrier, x, y)) = which_barrier(&signal, &barriers) else {
                    log::warn!("activated without a cursor position: {signal:?}");
                    continue;
                };
                let Some(barrier) = barrier else {
                    // On the edge, but not where a peer sits: straight back.
                    let (x, y) = nudged_inward(&barriers, x, y);
                    let options = ashpd::desktop::input_capture::ReleaseOptions::default()
                        .set_activation_id(signal.activation_id())
                        .set_cursor_position(Some((x, y)));
                    if let Err(error) = portal.release(&session, options).await {
                        log::warn!("Release: {error}");
                    }
                    continue;
                };
                activation = Some(signal.activation_id());
                report(CaptureEvent::Activated { barrier, x, y });
            }
            signal = deactivated.next() => {
                if signal.is_none() {
                    report(CaptureEvent::Lost("Deactivated signal stream ended".into()));
                    break;
                }
                activation = None;
                report(CaptureEvent::Deactivated);
            }
            signal = disabled.next() => {
                if signal.is_none() {
                    report(CaptureEvent::Lost("Disabled signal stream ended".into()));
                    break;
                }
                // The compositor suspended the session (barriers changed,
                // a display came or went): the same session goes on.
                activation = None;
                if let Err(error) = apply_barriers(&portal, &session, &zones, &barriers).await {
                    report(CaptureEvent::Lost(format!("Enable after Disabled: {error}")));
                    break;
                }
            }
            signal = zones_changed.next() => {
                if signal.is_none() {
                    report(CaptureEvent::Lost("ZonesChanged signal stream ended".into()));
                    break;
                }
                match fetch_zones(&portal, &session).await {
                    Ok(new) => {
                        zones = new;
                        report(CaptureEvent::Zones(displays_of(&zones)));
                        // The agent answers with barriers for the new edges;
                        // meanwhile the old ones are re-set against the new
                        // zone set so nothing stays suspended.
                        if let Err(error) = apply_barriers(&portal, &session, &zones, &barriers).await {
                            log::warn!("barriers after zone change: {error}");
                        }
                    }
                    Err(error) => {
                        report(CaptureEvent::Lost(format!("GetZones: {error}")));
                        break;
                    }
                }
            }
            event = ei_events.next() => {
                let event = match event {
                    Some(Ok(event)) => event,
                    Some(Err(error)) => {
                        report(CaptureEvent::Lost(format!("EIS: {error}")));
                        break;
                    }
                    None => {
                        report(CaptureEvent::Lost("EIS connection closed".into()));
                        break;
                    }
                };
                if let Some(lost) = handle_ei_event(event, &context, &report) {
                    report(CaptureEvent::Lost(lost));
                    break;
                }
            }
        }
    }
    let _ = portal.disable(&session, Default::default()).await;
    let _ = session.close().await;
}

/// A session, with persistence (InputCapture v2) where the portal has it.
async fn open_session(
    portal: &InputCapture,
    token: &TokenFile,
    report: &impl Fn(CaptureEvent),
) -> Result<Session<InputCapture>, CaptureEvent> {
    let capabilities = Capabilities::Keyboard | Capabilities::Pointer;
    if portal.version() >= 2 {
        let session = portal
            .create_session2(CreateSession2Options::default())
            .await
            .map_err(|error| CaptureEvent::Lost(format!("CreateSession2: {error}")))?;
        let restore = token.read();
        if restore.is_none() {
            report(CaptureEvent::Prompting);
        }
        let options = StartOptions::default()
            .set_capabilities(capabilities)
            .set_persist_mode(PersistMode::ExplicitlyRevoked)
            .set_restore_token(restore);
        let response = portal
            .start(&session, None, options)
            .await
            .and_then(|request| request.response());
        match response {
            Ok(response) => {
                match response.restore_token() {
                    Some(new) => token.write(new),
                    None => token.forget(),
                }
                Ok(session)
            }
            Err(error) => {
                token.forget();
                let _ = session.close().await;
                Err(classify(error, "Start"))
            }
        }
    } else {
        report(CaptureEvent::Prompting);
        let options = CreateSessionOptions::default().set_capabilities(capabilities);
        portal
            .create_session(None, options)
            .await
            .map(|(session, _)| session)
            .map_err(|error| classify(error, "CreateSession"))
    }
}

/// A refusal by the user is `Denied`; anything else `Lost`.
fn classify(error: ashpd::Error, what: &str) -> CaptureEvent {
    match error {
        ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => {
            CaptureEvent::Denied(format!("{what}: declined"))
        }
        other => CaptureEvent::Lost(format!("{what}: {other}")),
    }
}

async fn fetch_zones(portal: &InputCapture, session: &Session<InputCapture>) -> Result<Zones, ashpd::Error> {
    portal
        .zones(session, Default::default())
        .await?
        .response()
}

fn displays_of(zones: &Zones) -> Vec<Display> {
    zones
        .regions()
        .iter()
        .map(|region| Display {
            x: f64::from(region.x_offset()),
            y: f64::from(region.y_offset()),
            width: f64::from(region.width()),
            height: f64::from(region.height()),
            scale: 1.0,
        })
        .collect()
}

/// Sets the barriers and enables the session (`SetPointerBarriers`
/// suspends it). Without barriers the session is only disabled.
///
/// The portal gets one barrier per screen edge that any of the agent's
/// barriers lies on, spanning the whole edge: KDE's portal refuses a
/// barrier that does not fill the edge, Mutter accepts either. Which of
/// the agent's segments a hit falls in is decided here, on activation.
async fn apply_barriers(
    portal: &InputCapture,
    session: &Session<InputCapture>,
    zones: &Zones,
    barriers: &[Barrier],
) -> Result<(), ashpd::Error> {
    if barriers.is_empty() {
        return portal.disable(session, Default::default()).await;
    }
    let regions: Vec<(i32, i32, i32, i32)> = zones
        .regions()
        .iter()
        .map(|r| (r.x_offset(), r.y_offset(), r.width() as i32, r.height() as i32))
        .collect();
    let portal_barriers: Vec<PortalBarrier> = full_edges(&regions, barriers)
        .into_iter()
        .filter_map(|(id, line)| Some(PortalBarrier::new(NonZeroU32::new(id)?, line)))
        .collect();
    log::info!(
        "barriers to the portal: {:?} (zone set {})",
        portal_barriers.iter().map(|b| (b.barrier_id().get(), b.position().as_tuple())).collect::<Vec<_>>(),
        zones.zone_set()
    );
    let response = portal
        .set_pointer_barriers(session, &portal_barriers, zones.zone_set(), Default::default())
        .await?
        .response()?;
    for failed in response.failed_barriers() {
        log::warn!("the compositor refused barrier {failed}");
    }
    portal.enable(session, Default::default()).await
}

/// The portal's form of a barrier: both ends inclusive, on the outer edge
/// of a zone (`x + width` for the right edge, as the portal wants it).
fn portal_line(barrier: &Barrier) -> (i32, i32, i32, i32) {
    let line = barrier.line.round() as i32;
    let from = barrier.from.round() as i32;
    let to = (barrier.to.round() as i32 - 1).max(from);
    if barrier.side.is_vertical() {
        (line, from, line, to)
    } else {
        (from, line, to, line)
    }
}

/// One portal barrier per (zone, side) that the agent's barriers touch,
/// spanning that zone's whole edge; keyed by the first agent barrier on it
/// (the portal's barrier IDs only need to be unique).
fn full_edges(regions: &[(i32, i32, i32, i32)], barriers: &[Barrier]) -> Vec<(u32, (i32, i32, i32, i32))> {
    let mut edges: Vec<(u32, (i32, i32, i32, i32))> = Vec::new();
    for barrier in barriers {
        let Some(region) = regions.iter().find(|region| on_edge(barrier, **region)) else {
            // Not on any zone edge as the compositor sees it: sent as is,
            // and the portal says whether it takes it.
            edges.push((barrier.id, portal_line(barrier)));
            continue;
        };
        let (x, y, w, h) = *region;
        let full = Barrier {
            id: barrier.id,
            side: barrier.side,
            line: barrier.line,
            from: f64::from(if barrier.side.is_vertical() { y } else { x }),
            to: f64::from(if barrier.side.is_vertical() { y + h } else { x + w }),
        };
        let line = portal_line(&full);
        if !edges.iter().any(|(_, existing)| *existing == line) {
            edges.push((barrier.id, line));
        }
    }
    edges
}

/// Whether the barrier lies on an edge of this region `(x, y, w, h)`.
fn on_edge(barrier: &Barrier, region: (i32, i32, i32, i32)) -> bool {
    let (x, y, w, h) = (f64::from(region.0), f64::from(region.1), f64::from(region.2), f64::from(region.3));
    let near = |a: f64, b: f64| (a - b).abs() < 1.0;
    use rinflow_core::layout::Side;
    match barrier.side {
        Side::Left => near(barrier.line, x) && barrier.from < y + h && barrier.to > y,
        Side::Right => near(barrier.line, x + w) && barrier.from < y + h && barrier.to > y,
        Side::Top => near(barrier.line, y) && barrier.from < x + w && barrier.to > x,
        Side::Bottom => near(barrier.line, y + h) && barrier.from < x + w && barrier.to > x,
    }
}

/// Where the pointer hit, and which of the agent's barriers it falls in:
/// the portal's barrier covers a whole edge, so the segment is found by
/// position (the nearest one on that edge, within its extent). `None` for
/// the position when the compositor gave none; `None` for the barrier when
/// the hit is on a part of the edge no peer is placed against.
fn which_barrier(signal: &Activated, barriers: &[Barrier]) -> Option<(Option<u32>, f64, f64)> {
    let (x, y) = signal
        .cursor_position()
        .map(|(x, y)| (f64::from(x), f64::from(y)))?;
    let on_edge = |barrier: &Barrier| {
        let (along, across) = if barrier.side.is_vertical() { (y, x - barrier.line) } else { (x, y - barrier.line) };
        across.abs() <= 2.0 && along >= barrier.from - 1.0 && along < barrier.to + 1.0
    };
    let id = match signal.barrier_id() {
        // The portal's ID is the first segment on that edge; the hit may
        // be in another segment of the same edge.
        Some(ActivatedBarrier::Barrier(id)) if barriers.iter().any(|b| b.id == id.get() && on_edge(b)) => Some(id.get()),
        _ => barriers
            .iter()
            .filter(|barrier| on_edge(barrier))
            .min_by(|a, b| distance(a, x, y).total_cmp(&distance(b, x, y)))
            .map(|barrier| barrier.id),
    };
    Some((id, x, y))
}

/// Two points inward from the edge the hit was on.
fn nudged_inward(barriers: &[Barrier], x: f64, y: f64) -> (f64, f64) {
    use rinflow_core::layout::Side;
    let nearest = barriers
        .iter()
        .min_by(|a, b| distance(a, x, y).total_cmp(&distance(b, x, y)));
    match nearest.map(|b| b.side) {
        Some(Side::Right) => (x - 2.0, y),
        Some(Side::Left) => (x + 2.0, y),
        Some(Side::Bottom) => (x, y - 2.0),
        Some(Side::Top) => (x, y + 2.0),
        None => (x, y),
    }
}

fn distance(barrier: &Barrier, x: f64, y: f64) -> f64 {
    let (along, across) = if barrier.side.is_vertical() {
        (y, x - barrier.line)
    } else {
        (x, y - barrier.line)
    };
    let off = if along < barrier.from {
        barrier.from - along
    } else if along > barrier.to {
        along - barrier.to
    } else {
        0.0
    };
    across.hypot(off)
}

/// Forwards one EIS event; `Some(reason)` when the connection is over.
fn handle_ei_event(
    event: EiEvent,
    context: &ei::Context,
    report: &impl Fn(CaptureEvent),
) -> Option<String> {
    match event {
        EiEvent::SeatAdded(seat) => {
            seat.seat.bind_capabilities(
                DeviceCapability::Pointer
                    | DeviceCapability::PointerAbsolute
                    | DeviceCapability::Keyboard
                    | DeviceCapability::Button
                    | DeviceCapability::Scroll,
            );
            if let Err(error) = context.flush() {
                return Some(format!("EIS flush: {error}"));
            }
        }
        EiEvent::Disconnected(d) => {
            return Some(format!("EIS disconnected: {:?}", d.reason));
        }
        EiEvent::PointerMotion(m) => report(CaptureEvent::Motion {
            dx: f64::from(m.dx),
            dy: f64::from(m.dy),
        }),
        EiEvent::Button(b) => {
            if let Some(button) = button_number(b.button) {
                report(CaptureEvent::Button {
                    button,
                    down: b.state == ei::button::ButtonState::Press,
                });
            }
        }
        EiEvent::ScrollDelta(s) => {
            if s.dx != 0.0 || s.dy != 0.0 {
                // libei: positive is down / right. The core: toward the top
                // of the content / the left.
                report(CaptureEvent::Scroll {
                    dx: -f64::from(s.dx),
                    dy: -f64::from(s.dy),
                    precise: true,
                });
            }
        }
        EiEvent::ScrollDiscrete(s) => {
            if s.discrete_dx != 0 || s.discrete_dy != 0 {
                report(CaptureEvent::Scroll {
                    dx: -f64::from(s.discrete_dx) / 120.0 * POINTS_PER_NOTCH,
                    dy: -f64::from(s.discrete_dy) / 120.0 * POINTS_PER_NOTCH,
                    precise: false,
                });
            }
        }
        EiEvent::KeyboardKey(k) => {
            if let Some(usage) = hid_usage(k.key) {
                report(CaptureEvent::Key {
                    usage,
                    down: k.state == ei::keyboard::KeyState::Press,
                });
            }
        }
        _ => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rinflow_core::layout::Side;

    #[test]
    fn barriers_sit_on_the_outer_edge_inclusive() {
        let right = Barrier { id: 1, side: Side::Right, line: 1920.0, from: 0.0, to: 1080.0 };
        assert_eq!(portal_line(&right), (1920, 0, 1920, 1079));
        let top = Barrier { id: 2, side: Side::Top, line: 0.0, from: 100.0, to: 500.0 };
        assert_eq!(portal_line(&top), (100, 0, 499, 0));
    }

    #[test]
    fn barriers_are_widened_to_the_whole_edge_and_merged() {
        let regions = [(0, 0, 1920, 1080)];
        let barriers = [
            Barrier { id: 1, side: Side::Right, line: 1920.0, from: 90.0, to: 990.0 },
            Barrier { id: 2, side: Side::Right, line: 1920.0, from: 0.0, to: 80.0 },
            Barrier { id: 3, side: Side::Top, line: 0.0, from: 100.0, to: 500.0 },
        ];
        let edges = full_edges(&regions, &barriers);
        assert_eq!(edges, vec![(1, (1920, 0, 1920, 1079)), (3, (0, 0, 1919, 0))]);
    }

    #[test]
    fn the_nearest_barrier_stands_in_for_a_missing_id() {
        let barriers = [
            Barrier { id: 1, side: Side::Right, line: 1920.0, from: 0.0, to: 1080.0 },
            Barrier { id: 2, side: Side::Top, line: 0.0, from: 0.0, to: 1920.0 },
        ];
        assert!(distance(&barriers[0], 1919.0, 500.0) < distance(&barriers[1], 1919.0, 500.0));
        assert!(distance(&barriers[1], 900.0, 1.0) < distance(&barriers[0], 900.0, 1.0));
    }
}
