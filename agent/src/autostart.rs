//! Starting the agent when the user signs in: a desktop entry in
//! `~/.config/autostart`, which every desktop honours (outside a sandbox;
//! a Flatpak uses the Background portal instead).

use crate::paths;
use std::fs;
use std::io;
use std::path::PathBuf;

fn path() -> PathBuf {
    paths::autostart_directory().join(format!("{}.agent.desktop", paths::APP_ID))
}

pub fn enabled() -> bool {
    fs::read_to_string(path()).is_ok_and(|text| !text.contains("Hidden=true"))
}

pub fn set(enabled: bool) -> io::Result<()> {
    let path = path();
    if !enabled {
        return match fs::remove_file(&path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe()?;
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Rinflow Agent\nExec={}\nIcon={}\nNoDisplay=true\nX-GNOME-Autostart-Phase=Applications\nX-KDE-autostart-after=panel\n",
        exe.display(),
        paths::APP_ID
    );
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, entry)
}
