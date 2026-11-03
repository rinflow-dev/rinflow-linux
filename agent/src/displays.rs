//! The last displays the compositor described, kept between runs so the
//! core starts with the right geometry before the portal has answered
//! (and when it never does: no capture backend, as under gamescope).

use rinflow_core::layout::Display;
use rinflow_shell::store;
use std::path::{Path, PathBuf};

pub struct DisplayCache(PathBuf);

impl DisplayCache {
    pub fn new(state_directory: &Path) -> Self {
        Self(state_directory.join("displays.json"))
    }

    /// The saved displays, or one plain screen until something better
    /// is known.
    pub fn load(&self) -> Vec<Display> {
        store::read_json::<Vec<Display>>(&self.0)
            .filter(|displays| !displays.is_empty())
            .unwrap_or_else(|| vec![Display { x: 0.0, y: 0.0, width: 1920.0, height: 1080.0, scale: 1.0 }])
    }

    pub fn save(&self, displays: &[Display]) {
        if !displays.is_empty() {
            store::save_json(&self.0, "displays", &displays);
        }
    }
}
