//! Internet-facing protections: secure random tokens, brute-force lockout,
//! dashboard sessions, admin roles and an activity log.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;

/// Failed logins allowed per IP within `WINDOW` before it is locked out.
pub const MAX_FAILURES: u32 = 10;
pub const WINDOW: Duration = Duration::from_secs(15 * 60);
pub const LOCKOUT: Duration = Duration::from_secs(15 * 60);
pub const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);
pub const MIN_PASSWORD_LEN: usize = 12;
const EVENTS_KEPT: usize = 1000;

/// Cryptographically secure random bytes from the TLS library's RNG.
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    rustls::crypto::aws_lc_rs::default_provider()
        .secure_random
        .fill(&mut buf)
        .expect("system random number generator failed");
    buf
}

pub fn random_token() -> String {
    random_bytes(32)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A 20-character password without look-alike characters (~115 bits).
pub fn generate_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    // Rejection sampling keeps every character equally likely.
    let limit = 256 - (256 % ALPHABET.len());
    let mut out = String::with_capacity(20);
    while out.len() < 20 {
        for b in random_bytes(32) {
            if (b as usize) < limit && out.len() < 20 {
                out.push(ALPHABET[b as usize % ALPHABET.len()] as char);
            }
        }
    }
    out
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Default)]
struct Attempts {
    failures: u32,
    window_start: Option<Instant>,
    locked_until: Option<Instant>,
}

/// Per-IP lockout after repeated failed logins.
#[derive(Default)]
pub struct Limiter {
    ips: Mutex<HashMap<IpAddr, Attempts>>,
}

impl Limiter {
    pub fn is_locked(&self, ip: IpAddr) -> bool {
        let ips = self.ips.lock().unwrap_or_else(|e| e.into_inner());
        ips.get(&ip)
            .and_then(|a| a.locked_until)
            .is_some_and(|t| t > Instant::now())
    }

    /// Record a failure. Returns true if this failure triggered a lockout.
    pub fn fail(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut ips = self.ips.lock().unwrap_or_else(|e| e.into_inner());
        ips.retain(|_, a| {
            a.locked_until.is_some_and(|t| t > now)
                || a.window_start
                    .is_some_and(|t| now.duration_since(t) < WINDOW)
        });
        let a = ips.entry(ip).or_default();
        if a.window_start
            .is_none_or(|t| now.duration_since(t) >= WINDOW)
        {
            a.window_start = Some(now);
            a.failures = 0;
        }
        a.failures += 1;
        if a.failures >= MAX_FAILURES && a.locked_until.is_none_or(|t| t <= now) {
            a.locked_until = Some(now + LOCKOUT);
            return true;
        }
        false
    }

    pub fn succeed(&self, ip: IpAddr) {
        let mut ips = self.ips.lock().unwrap_or_else(|e| e.into_inner());
        ips.remove(&ip);
    }

    pub fn locked_count(&self) -> usize {
        let now = Instant::now();
        let ips = self.ips.lock().unwrap_or_else(|e| e.into_inner());
        ips.values()
            .filter(|a| a.locked_until.is_some_and(|t| t > now))
            .count()
    }
}

/// Dashboard sessions, held in memory (a restart signs everyone out).
#[derive(Default)]
pub struct Sessions {
    map: Mutex<HashMap<String, (String, Instant)>>,
}

impl Sessions {
    pub fn create(&self, user: &str) -> String {
        let token = random_token();
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        map.retain(|_, (_, exp)| *exp > now);
        map.insert(token.clone(), (user.to_string(), now + SESSION_TTL));
        token
    }

    pub fn user(&self, token: &str) -> Option<String> {
        let map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        map.get(token)
            .filter(|(_, exp)| *exp > Instant::now())
            .map(|(u, _)| u.clone())
    }

    pub fn remove(&self, token: &str) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(token);
    }

    /// Sign a user out everywhere (after a password change or removal),
    /// except the session `keep`.
    pub fn remove_user(&self, user: &str, keep: Option<&str>) {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|t, (u, _)| u != user || Some(t.as_str()) == keep);
    }
}

/// Admin user names, one per line, in a file next to the users file.
pub fn admins_path(htpasswd: &Path) -> PathBuf {
    htpasswd.with_file_name(".admins")
}

pub fn load_admins(path: &Path) -> HashSet<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

pub fn set_admin(path: &Path, user: &str, admin: bool) -> std::io::Result<()> {
    let mut admins: Vec<String> = load_admins(path)
        .into_iter()
        .filter(|u| u != user)
        .collect();
    if admin {
        admins.push(user.to_string());
    }
    admins.sort();
    crate::auth::write_private(path, &(admins.join("\n") + "\n"))
}

#[derive(Clone)]
pub struct Event {
    pub time: u64,
    pub user: Option<String>,
    pub ip: IpAddr,
    pub kind: &'static str,
    pub detail: String,
}

impl Event {
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "time": self.time,
            "user": self.user,
            "ip": self.ip.to_string(),
            "kind": self.kind,
            "detail": self.detail,
        })
    }
}

/// In-memory activity log of the most recent events.
#[derive(Default)]
pub struct Activity {
    events: Mutex<VecDeque<Event>>,
    last_seen: Mutex<HashMap<String, u64>>,
}

impl Activity {
    pub fn record(
        &self,
        user: Option<&str>,
        ip: IpAddr,
        kind: &'static str,
        detail: impl Into<String>,
    ) {
        let mut events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        if events.len() >= EVENTS_KEPT {
            events.pop_front();
        }
        events.push_back(Event {
            time: unix_now(),
            user: user.map(str::to_string),
            ip,
            kind,
            detail: detail.into(),
        });
    }

    pub fn seen(&self, user: &str) {
        self.last_seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(user.to_string(), unix_now());
    }

    pub fn last_seen(&self, user: &str) -> Option<u64> {
        self.last_seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(user)
            .copied()
    }

    /// Newest first, optionally only one user's events.
    pub fn recent(&self, only_user: Option<&str>, limit: usize) -> Vec<Event> {
        let events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        events
            .iter()
            .rev()
            .filter(|e| only_user.is_none_or(|u| e.user.as_deref() == Some(u)))
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn count_since(&self, kind: &str, since: u64) -> usize {
        let events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        events
            .iter()
            .filter(|e| e.kind == kind && e.time >= since)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_are_random_and_long() {
        let a = generate_password();
        let b = generate_password();
        assert_eq!(a.len(), 20);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn lockout_after_max_failures() {
        let l = Limiter::default();
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        for _ in 0..MAX_FAILURES - 1 {
            assert!(!l.fail(ip));
        }
        assert!(!l.is_locked(ip));
        assert!(l.fail(ip));
        assert!(l.is_locked(ip));
        assert_eq!(l.locked_count(), 1);
        let other: IpAddr = "203.0.113.10".parse().unwrap();
        assert!(!l.is_locked(other));
    }

    #[test]
    fn sessions_expire_on_removal() {
        let s = Sessions::default();
        let t1 = s.create("rob");
        let t2 = s.create("rob");
        assert_eq!(s.user(&t1).as_deref(), Some("rob"));
        s.remove_user("rob", Some(&t2));
        assert_eq!(s.user(&t1), None);
        assert_eq!(s.user(&t2).as_deref(), Some("rob"));
    }
}
