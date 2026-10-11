//! Mount guard: refuse to write to a path that is on the system disk.
//!
//! An unplugged USB drive leaves its mount point as a plain directory (or no
//! directory at all). Without this check a backup would silently land on the
//! internal disk.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Volumes that count as "the system disk". On macOS the root volume is a
/// read-only system volume and user data lives on a separate Data volume, so
/// both are listed.
const SYSTEM_VOLUMES: &[&str] = &["/", "/System/Volumes/Data"];

/// Fail if `path` (or its nearest existing ancestor) is on a system volume.
pub fn check_mounted(path: &Path) -> Result<()> {
    let existing = nearest_existing(path);
    let dev = std::fs::metadata(&existing)
        .map_err(|e| Error::io(format!("cannot stat {}", existing.display()), e))?
        .dev();
    let on_system = SYSTEM_VOLUMES
        .iter()
        .filter_map(|v| std::fs::metadata(v).ok())
        .any(|m| m.dev() == dev);
    if on_system {
        return Err(Error::NotMounted {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn nearest_existing(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    while !p.exists() {
        if !p.pop() {
            return PathBuf::from("/");
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_and_home_are_not_mounted_drives() {
        assert!(check_mounted(Path::new("/")).is_err());
        assert!(check_mounted(Path::new("/definitely/not/here")).is_err());
        if let Some(home) = std::env::var_os("HOME") {
            assert!(check_mounted(Path::new(&home)).is_err());
        }
    }
}
