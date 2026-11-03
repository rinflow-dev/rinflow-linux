//! Finding a compositor's own EIS socket, for desktops without the
//! RemoteDesktop portal: gamescope (Steam Deck's Gaming Mode) listens on
//! `$XDG_RUNTIME_DIR/gamescope-<n>-ei` and lets any process of the same
//! user connect. `LIBEI_SOCKET` names it for processes gamescope started;
//! an agent started by systemd or a plugin has to look.

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

/// The socket to try, if any: `LIBEI_SOCKET` when set, else the first
/// `*-ei` socket owned by this user in the runtime directory, gamescope's
/// first.
pub fn find() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", uid())));
    if let Some(name) = std::env::var_os("LIBEI_SOCKET") {
        let path = PathBuf::from(&name);
        let path = if path.is_absolute() {
            path
        } else {
            runtime.join(path)
        };
        if is_our_socket(&path) {
            return Some(path);
        }
    }
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(&runtime)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("-ei"))
                && is_our_socket(path)
        })
        .collect();
    candidates.sort_by_key(|path| {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        (!name.starts_with("gamescope"), name.to_owned())
    });
    candidates.into_iter().next()
}

fn is_our_socket(path: &std::path::Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| {
        use std::os::unix::fs::FileTypeExt;
        meta.file_type().is_socket() && meta.uid() == uid()
    })
}

fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}
