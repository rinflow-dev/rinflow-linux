//! Small JSON files in the agent's data directory, written so that a crash
//! never leaves one half written.

use std::fs;
use std::io;
use std::path::Path;

/// Replaces a file without ever leaving it half written: a crash or power
/// loss mid-write would otherwise make it unreadable and lose it.
pub fn write_file(path: &Path, text: &str) -> io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        // On disk before it replaces the old file, not only in the cache.
        file.sync_all()?;
    }
    fs::rename(&temporary, path)
}

/// Reads a JSON file; `None` when there is none. One that cannot be read is
/// moved aside to `<name>.corrupt` and the defaults are used, so the next
/// save does not overwrite what may still be recovered by hand.
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(value) => Some(value),
        Err(error) => {
            let mut aside = path.as_os_str().to_owned();
            aside.push(".corrupt");
            log::warn!(
                "{} is unreadable ({error}); kept as {} and using defaults",
                path.display(),
                Path::new(&aside).display()
            );
            let _ = fs::rename(path, &aside);
            None
        }
    }
}

/// Saves a value as pretty JSON, logging rather than failing: a settings
/// file that cannot be written must not stop the agent.
pub fn save_json<T: serde::Serialize>(path: &Path, what: &str, value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(text) => {
            if let Err(error) = write_file(path, &text) {
                log::warn!("cannot save {what}: {error}");
            }
        }
        Err(error) => log::warn!("cannot encode {what}: {error}"),
    }
}
