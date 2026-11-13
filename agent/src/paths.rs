//! Where the agent keeps things, by the XDG base directory convention.

use std::path::PathBuf;

/// The name shared by the data, state and runtime directories.
pub const APP_DIR: &str = "rinflow";
/// The application ID, as the desktop file and the window know it.
pub const APP_ID: &str = "com.rinflow.rinflow";

fn xdg(variable: &str, fallback: &[&str]) -> PathBuf {
    if let Some(value) = std::env::var_os(variable).filter(|v| !v.is_empty()) {
        return PathBuf::from(value);
    }
    let mut path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    for part in fallback {
        path.push(part);
    }
    path
}

/// Identity, pairings, layout, settings: `~/.local/share/rinflow`.
pub fn data_directory() -> PathBuf {
    xdg("XDG_DATA_HOME", &[".local", "share"]).join(APP_DIR)
}

/// Portal restore tokens and the last known displays: `~/.local/state/rinflow`.
pub fn state_directory() -> PathBuf {
    xdg("XDG_STATE_HOME", &[".local", "state"]).join(APP_DIR)
}

/// What lives only while the session does: `$XDG_RUNTIME_DIR/rinflow`.
pub fn runtime_directory() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // SAFETY: getuid has no preconditions.
            let uid = unsafe { libc::getuid() };
            PathBuf::from(format!("/run/user/{uid}"))
        });
    runtime.join(APP_DIR)
}

/// The window's socket: `$XDG_RUNTIME_DIR/rinflow/agent.sock`.
pub fn socket_path() -> PathBuf {
    runtime_directory().join("agent.sock")
}

/// `~/.config/autostart`.
pub fn autostart_directory() -> PathBuf {
    xdg("XDG_CONFIG_HOME", &[".config"]).join("autostart")
}

/// This machine's name, as the other devices will list it.
pub fn host_name() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: the buffer is valid for its length; gethostname writes a NUL
    // terminated name into it.
    let result = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if result == 0 {
        let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
        let name = String::from_utf8_lossy(&buffer[..end]).trim().to_owned();
        if !name.is_empty() && name != "localhost" {
            return name;
        }
    }
    "Linux PC".to_owned()
}
