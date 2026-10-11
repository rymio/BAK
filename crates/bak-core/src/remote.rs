//! Remote model: a name plus a URL whose form selects the backend.

use std::fs;
use std::path::{Path, PathBuf};

use crate::config::RemoteConfig;
use crate::error::{Error, Result};
use crate::redact::Secret;

/// Pack size used by default for folders that a sync client (iCloud Drive,
/// Google Drive, Dropbox, OneDrive) uploads: fewer, larger files sync better.
pub const CLOUD_FOLDER_PACK_SIZE_MIB: u32 = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// A directory: internal disk, mounted USB drive, or a cloud-synced folder.
    Local(PathBuf),
    /// A restic `rest-server`. `url` never contains credentials.
    Rest { url: String },
}

impl Location {
    pub fn parse(url: &str) -> Result<Self> {
        let bad = |reason: &str| Error::RemoteUrl {
            url: crate::redact::scrub(url),
            reason: reason.to_string(),
        };
        if let Some(rest) = url.strip_prefix("rest:") {
            let after_scheme = rest
                .strip_prefix("https://")
                .or_else(|| rest.strip_prefix("http://"))
                .ok_or_else(|| bad("expected rest:https://host:port/path"))?;
            let authority = after_scheme.split('/').next().unwrap_or("");
            if authority.contains('@') {
                return Err(bad(
                    "credentials must not be in the url; use BAK_REST_USER/BAK_REST_PASSWORD \
                     or credentials_file",
                ));
            }
            if authority.is_empty() {
                return Err(bad("missing host"));
            }
            return Ok(Self::Rest {
                url: rest.to_string(),
            });
        }
        if let Some(path) = url.strip_prefix("local:") {
            return Ok(Self::Local(expand_tilde(path)));
        }
        if url.starts_with('/') || url.starts_with('~') {
            return Ok(Self::Local(expand_tilde(url)));
        }
        Err(bad(
            "expected an absolute path, ~/path, local:<path> or rest:https://host:port/path",
        ))
    }
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// Whether `path` is inside a folder that a cloud sync client uploads.
pub fn is_cloud_synced_folder(path: &Path) -> bool {
    let s = path.to_string_lossy();
    // macOS: iCloud Drive and File Provider clients (Google Drive, OneDrive, Dropbox, Box).
    s.contains("/Library/Mobile Documents/")
        || s.contains("/Library/CloudStorage/")
        // Older standalone clients and Linux/rclone conventions.
        || s.contains("/Google Drive/")
        || s.contains("/Dropbox/")
        || s.contains("/OneDrive/")
}

#[derive(Debug, Clone)]
pub struct RestCredentials {
    pub username: String,
    pub password: Secret,
}

/// Everything an engine needs to reach one remote.
#[derive(Debug, Clone)]
pub struct RemoteSpec {
    pub name: String,
    pub location: Location,
    pub require_mount: bool,
    pub cacert: Option<PathBuf>,
    pub pack_size_mib: Option<u32>,
    pub credentials: Option<RestCredentials>,
}

impl RemoteSpec {
    /// Build a spec from config, reading REST credentials from the environment
    /// or the remote's credentials file.
    pub fn from_config(name: &str, cfg: &RemoteConfig) -> Result<Self> {
        let location = Location::parse(&cfg.url)?;
        let credentials = match location {
            Location::Rest { .. } => rest_credentials(name, cfg)?,
            Location::Local(_) => None,
        };
        Ok(Self {
            name: name.to_string(),
            location,
            require_mount: cfg.require_mount,
            cacert: cfg.cacert.as_deref().map(expand_tilde),
            pack_size_mib: cfg.pack_size_mib,
            credentials,
        })
    }

    /// The URL to show the user. Never contains credentials.
    pub fn display_url(&self) -> String {
        match &self.location {
            Location::Local(p) => p.display().to_string(),
            Location::Rest { url } => format!("rest:{url}"),
        }
    }

    /// Fail unless the remote's path is on a mounted drive, when the remote requires it.
    pub fn check_mount(&self) -> Result<()> {
        match (&self.location, self.require_mount) {
            (Location::Local(path), true) => crate::mount::check_mounted(path),
            _ => Ok(()),
        }
    }
}

fn env_suffix(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Credentials come from `BAK_REST_USER_<NAME>`/`BAK_REST_PASSWORD_<NAME>`, then
/// `BAK_REST_USER`/`BAK_REST_PASSWORD`, then the remote's `credentials_file`.
fn rest_credentials(name: &str, cfg: &RemoteConfig) -> Result<Option<RestCredentials>> {
    let suffix = env_suffix(name);
    let var = |base: &str| {
        std::env::var(format!("{base}_{suffix}"))
            .or_else(|_| std::env::var(base))
            .ok()
    };
    if let Some(password) = var("BAK_REST_PASSWORD") {
        let password = Secret::new(password);
        let username = var("BAK_REST_USER").unwrap_or_default();
        crate::redact::register(&username);
        return Ok(Some(RestCredentials { username, password }));
    }
    let Some(file) = cfg.credentials_file.as_deref() else {
        return Ok(None);
    };
    let path = expand_tilde(file);
    check_private_file(&path)?;
    let text = fs::read_to_string(&path)
        .map_err(|e| Error::io(format!("cannot read {}", path.display()), e))?;
    let line = text.lines().next().unwrap_or("").trim_end();
    let (username, password) = line.split_once(':').ok_or_else(|| {
        Error::Config(format!(
            "{} must contain one line `username:password`",
            path.display()
        ))
    })?;
    crate::redact::register(username);
    Ok(Some(RestCredentials {
        username: username.to_string(),
        password: Secret::new(password),
    }))
}

/// Refuse a secrets file that other users can read.
pub fn check_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta =
        fs::metadata(path).map_err(|e| Error::io(format!("cannot read {}", path.display()), e))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(Error::Config(format!(
            "{} has mode {mode:o}; it must be 0600 (chmod 600 {})",
            path.display(),
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_url_forms() {
        assert_eq!(
            Location::parse("/mnt/backup/x").unwrap(),
            Location::Local("/mnt/backup/x".into())
        );
        assert_eq!(
            Location::parse("rest:https://h:8000/p").unwrap(),
            Location::Rest {
                url: "https://h:8000/p".into()
            }
        );
        assert!(Location::parse("rest:https://u:pw@h:8000/p").is_err());
        assert!(Location::parse("relative/path").is_err());
        assert!(Location::parse("sftp:host:/x").is_err());
    }

    #[test]
    fn detects_cloud_folders() {
        assert!(is_cloud_synced_folder(Path::new(
            "/Users/r/Library/Mobile Documents/com~apple~CloudDocs/bak"
        )));
        assert!(is_cloud_synced_folder(Path::new(
            "/Users/r/Library/CloudStorage/GoogleDrive-r@x.com/My Drive/bak"
        )));
        assert!(!is_cloud_synced_folder(Path::new("/Volumes/USB1/bak")));
    }
}
