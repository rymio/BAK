//! htpasswd authentication (bcrypt entries, as written by `htpasswd -B` or
//! `bak-server user add`).
//!
//! bcrypt is deliberately slow, and a backup makes many requests, so a
//! successful login is cached as a SHA-256 of the password until the file
//! changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use base64::Engine as _;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

pub const BCRYPT_COST: u32 = 12;

pub struct Htpasswd {
    path: PathBuf,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    mtime: Option<SystemTime>,
    users: HashMap<String, String>,
    verified: HashMap<String, [u8; 32]>,
}

impl Htpasswd {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            inner: Mutex::new(Inner::default()),
        }
    }

    pub async fn verify(&self, user: &str, password: &str) -> std::io::Result<bool> {
        let digest: [u8; 32] = Sha256::digest(password.as_bytes()).into();
        let hash = {
            let mut inner = self.inner.lock().await;
            let mtime = tokio::fs::metadata(&self.path).await?.modified()?;
            if inner.mtime != Some(mtime) {
                let text = tokio::fs::read_to_string(&self.path).await?;
                inner.users = parse(&text);
                inner.verified.clear();
                inner.mtime = Some(mtime);
            }
            if inner.verified.get(user) == Some(&digest) {
                return Ok(true);
            }
            match inner.users.get(user) {
                Some(h) => h.clone(),
                None => return Ok(false),
            }
        };
        let pw = password.to_string();
        let ok = tokio::task::spawn_blocking(move || bcrypt::verify(pw, &hash).unwrap_or(false))
            .await
            .map_err(std::io::Error::other)?;
        if ok {
            self.inner
                .lock()
                .await
                .verified
                .insert(user.to_string(), digest);
        }
        Ok(ok)
    }
}

/// `user:hash` lines; blank lines and `#` comments are ignored.
pub fn parse(text: &str) -> HashMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once(':'))
        .map(|(u, h)| (u.to_string(), h.to_string()))
        .collect()
}

/// Decode an `Authorization: Basic …` header value.
pub fn parse_basic(value: &str) -> Option<(String, String)> {
    let encoded = value.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (u, p) = text.split_once(':')?;
    Some((u.to_string(), p.to_string()))
}

pub fn valid_username(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Add or replace a user in an htpasswd file (created with mode 0600).
pub fn set_user(path: &Path, user: &str, password: &str) -> std::io::Result<()> {
    let hash = bcrypt::hash(password, BCRYPT_COST).map_err(std::io::Error::other)?;
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| l.split_once(':').map(|(u, _)| u) != Some(user))
        .map(str::to_string)
        .collect();
    lines.push(format!("{user}:{hash}"));
    write_private(path, &(lines.join("\n") + "\n"))
}

/// Remove a user. Returns whether the user existed.
pub fn remove_user(path: &Path, user: &str) -> std::io::Result<bool> {
    let text = std::fs::read_to_string(path)?;
    let before = text.lines().count();
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.split_once(':').map(|(u, _)| u) != Some(user))
        .collect();
    let removed = lines.len() < before;
    write_private(path, &(lines.join("\n") + "\n"))?;
    Ok(removed)
}

pub fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_header() {
        // "rob:p:w" — the password may contain a colon.
        assert_eq!(
            parse_basic("Basic cm9iOnA6dw=="),
            Some(("rob".into(), "p:w".into()))
        );
        assert_eq!(parse_basic("Bearer x"), None);
    }

    #[tokio::test]
    async fn add_verify_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".htpasswd");
        set_user(&path, "rob", "secret").unwrap();
        let h = Htpasswd::new(&path);
        assert!(h.verify("rob", "secret").await.unwrap());
        assert!(h.verify("rob", "secret").await.unwrap()); // cached
        assert!(!h.verify("rob", "wrong").await.unwrap());
        assert!(!h.verify("eve", "secret").await.unwrap());
        assert!(remove_user(&path, "rob").unwrap());
        // Ensure the mtime changes even on coarse-grained filesystems.
        let later = SystemTime::now() + std::time::Duration::from_secs(2);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(!h.verify("rob", "secret").await.unwrap());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
