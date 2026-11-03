//! The device ID and name, and where the agent keeps its files.

use crate::store::write_file;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub struct Identity {
    pub device_id: String,
    pub device_name: String,
}

/// Loads the identity from `directory`, creating the directory and a device
/// ID on first use. `device_name` is the host's name as the shell found it.
pub fn load(directory: &Path, device_name: String) -> io::Result<Identity> {
    fs::create_dir_all(directory)?;
    Ok(Identity {
        device_id: load_or_create_device_id(directory)?,
        device_name,
    })
}

/// Where this device's key pair and certificate live (see the core's
/// `secure.rs`): readable by this user only, like the rest of the folder.
pub fn identity_path(directory: &Path) -> PathBuf {
    directory.join("identity.pem")
}

/// The device ID, made on first use. Only a missing (or empty) file gets a
/// new one: a file that cannot be read now (a disk error) is an error.
/// Pairings and placements on every peer are keyed by this ID, so a new one
/// would turn this device into a stranger to all of them.
fn load_or_create_device_id(directory: &Path) -> io::Result<String> {
    let path = directory.join("device-id");
    match fs::read_to_string(&path) {
        Ok(stored) if !stored.trim().is_empty() => return Ok(stored.trim().to_owned()),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "cannot read {} ({error}); it is kept, not replaced",
                    path.display()
                ),
            ));
        }
    }
    let id = new_id();
    write_file(&path, &id)?;
    Ok(id)
}

/// A random 128-bit ID in UUID form, from the operating system's random
/// source.
fn new_id() -> String {
    let mut bytes = [0u8; 16];
    fill_random(&mut bytes);
    // Version 4, variant 1, so it reads as a UUID wherever one is expected.
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn fill_random(bytes: &mut [u8]) {
    use std::io::Read;
    if let Ok(mut urandom) = fs::File::open("/dev/urandom")
        && urandom.read_exact(bytes).is_ok()
    {
        return;
    }
    // Without a random device (never on a real system): the clock and the
    // address of a stack value, hashed. Still unique enough for an ID that
    // only has to differ between a handful of a user's own machines.
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    let mut seed = hasher.finish();
    for byte in bytes {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *byte = (seed >> 56) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_uuids_and_persist() {
        let directory = std::env::temp_dir().join(format!("rinflow-shell-{}", new_id()));
        let first = load(&directory, "box".into()).unwrap();
        assert_eq!(first.device_id.len(), 36);
        assert_eq!(first.device_id.as_bytes()[14], b'4');
        let second = load(&directory, "box".into()).unwrap();
        assert_eq!(first.device_id, second.device_id);
        let _ = fs::remove_dir_all(directory);
    }
}
