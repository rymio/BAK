//! Process-wide secret redaction.
//!
//! Every secret `bak` reads is registered here when it is wrapped in a
//! [`Secret`]. Log lines and error messages pass through [`scrub`] before they
//! are printed, so a library that echoes a URL with credentials in it cannot
//! leak them.

use std::fmt;
use std::sync::{Mutex, OnceLock};

const MASK: &str = "***";

fn registry() -> &'static Mutex<Vec<String>> {
    static REGISTRY: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a value that must never be printed, plus its URL-encoded form.
pub fn register(secret: &str) {
    if secret.is_empty() {
        return;
    }
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    for form in [secret.to_string(), percent_encode_userinfo(secret)] {
        if !reg.contains(&form) {
            reg.push(form);
        }
    }
    // Replace longer secrets first so a secret that contains another is fully masked.
    reg.sort_by_key(|s| std::cmp::Reverse(s.len()));
}

/// Replace every registered secret in `text` with `***`.
pub fn scrub(text: &str) -> String {
    let reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    let mut out = text.to_string();
    for secret in reg.iter() {
        if out.contains(secret.as_str()) {
            out = out.replace(secret.as_str(), MASK);
        }
    }
    out
}

/// Percent-encode a string for the userinfo part of a URL (RFC 3986).
pub fn percent_encode_userinfo(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A secret string. It registers itself for redaction and never prints its value.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        register(&value);
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_raw_and_encoded_forms() {
        let s = Secret::new("p@ss word");
        let text = "url https://u:p%40ss%20word@host and p@ss word";
        let out = scrub(text);
        assert!(!out.contains(s.expose()));
        assert!(!out.contains("p%40ss%20word"));
        assert_eq!(format!("{s:?}"), "Secret(***)");
    }
}
