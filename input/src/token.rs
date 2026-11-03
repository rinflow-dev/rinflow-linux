//! Restore tokens of the portals. A token lets a session start again
//! without asking the user; each one is valid once, so every start's new
//! token replaces the file.

use std::fs;
use std::path::{Path, PathBuf};

pub struct TokenFile(PathBuf);

impl TokenFile {
    /// `<state directory>/<name>.token`.
    pub fn new(state_directory: &Path, name: &str) -> Self {
        Self(state_directory.join(format!("{name}.token")))
    }

    pub fn read(&self) -> Option<String> {
        let token = fs::read_to_string(&self.0).ok()?;
        let token = token.trim();
        (!token.is_empty()).then(|| token.to_owned())
    }

    pub fn write(&self, token: &str) {
        let write = || -> std::io::Result<()> {
            if let Some(parent) = self.0.parent() {
                fs::create_dir_all(parent)?;
            }
            let temporary = self.0.with_extension("tmp");
            fs::write(&temporary, token)?;
            fs::rename(&temporary, &self.0)
        };
        if let Err(error) = write() {
            log::warn!("cannot save {}: {error}", self.0.display());
        }
    }

    /// After the desktop refused a token (revoked, or from another
    /// desktop): the next start asks the user afresh.
    pub fn forget(&self) {
        let _ = fs::remove_file(&self.0);
    }
}
