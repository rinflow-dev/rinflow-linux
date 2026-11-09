//! The libei handshake over a file descriptor, shared by capture and
//! emulation.

use reis::ei;
use reis::event::Connection;
use reis::tokio::EiConvertEventStream;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;

pub const CLIENT_NAME: &str = "com.rinflow.rinflow";

pub async fn handshake(
    fd: OwnedFd,
    kind: ei::handshake::ContextType,
) -> Result<(ei::Context, Connection, EiConvertEventStream), String> {
    let stream = UnixStream::from(fd);
    stream
        .set_nonblocking(true)
        .map_err(|error| format!("EIS socket: {error}"))?;
    let context = ei::Context::new(stream).map_err(|error| format!("EIS context: {error}"))?;
    let (connection, events) = context
        .handshake_tokio(CLIENT_NAME, kind)
        .await
        .map_err(|error| format!("EIS handshake: {error}"))?;
    Ok((context, connection, events))
}

/// libei timestamps are CLOCK_MONOTONIC microseconds. KWin checks them;
/// Mutter does not, which is how the wall clock went unnoticed there.
pub fn now_micros() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime writes a timespec into the pointer given.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } != 0 {
        return 0;
    }
    ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1000
}
