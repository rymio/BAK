//! TLS: load a certificate and key, or generate a private CA plus a server
//! certificate signed by it.
//!
//! Clients trust the CA (`ca.pem`, passed to `bak remote add --cacert`), not
//! the server certificate. The server certificate is valid for 397 days, as
//! macOS requires, and is renewed from the same CA when fewer than 30 days
//! remain, so clients never need a new file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, SanType,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

const SERVER_DAYS: u64 = 397;
const CA_DAYS: u64 = 3650;
const RENEW_DAYS: u64 = 30;
const DAY: u64 = 86_400;

macro_rules! date {
    ($secs:expr) => {{
        let (y, m, d) = ymd($secs);
        rcgen::date_time_ymd(y, m, d)
    }};
}

pub struct Generated {
    pub ca: PathBuf,
    pub cert: PathBuf,
    pub key: PathBuf,
    /// Whether a new server certificate was written on this call.
    pub renewed: bool,
}

/// Make sure `<dir>/ca.pem`, `cert.pem` and `key.pem` exist and are current.
/// `hosts` are the DNS names and IP addresses clients use to reach the server.
pub fn ensure_generated(dir: &Path, hosts: &[String]) -> std::io::Result<Generated> {
    let err = |e: rcgen::Error| std::io::Error::other(e.to_string());
    std::fs::create_dir_all(dir)?;
    let (ca_cert, ca_key, cert, key, expires) = (
        dir.join("ca.pem"),
        dir.join("ca-key.pem"),
        dir.join("cert.pem"),
        dir.join("key.pem"),
        dir.join("cert.expires"),
    );

    let ca_signing_key = if ca_key.exists() && ca_cert.exists() {
        KeyPair::from_pem(&std::fs::read_to_string(&ca_key)?).map_err(err)?
    } else {
        let key = KeyPair::generate().map_err(err)?;
        let ca = ca_params()?.self_signed(&key).map_err(err)?;
        crate::auth::write_private(&ca_key, &key.serialize_pem())?;
        std::fs::write(&ca_cert, ca.pem())?;
        key
    };

    let now = unix_now();
    let current_until: Option<u64> = std::fs::read_to_string(&expires)
        .ok()
        .and_then(|s| s.trim().parse().ok());
    let hosts_file = dir.join("cert.hosts");
    let same_hosts =
        std::fs::read_to_string(&hosts_file).ok().as_deref() == Some(hosts.join(",").as_str());
    let fresh = cert.exists()
        && key.exists()
        && same_hosts
        && current_until.is_some_and(|t| t > now + RENEW_DAYS * DAY);
    if fresh {
        return Ok(Generated {
            ca: ca_cert,
            cert,
            key,
            renewed: false,
        });
    }

    let issuer = Issuer::new(ca_params()?, ca_signing_key);
    let mut params = CertificateParams::new(Vec::<String>::new()).map_err(err)?;
    for h in hosts {
        params
            .subject_alt_names
            .push(match h.parse::<std::net::IpAddr>() {
                Ok(ip) => SanType::IpAddress(ip),
                Err(_) => SanType::DnsName(h.clone().try_into().map_err(err)?),
            });
    }
    let mut dn = DistinguishedName::new();
    dn.push(
        DnType::CommonName,
        hosts
            .first()
            .cloned()
            .unwrap_or_else(|| "bak-server".into()),
    );
    params.distinguished_name = dn;
    params.not_before = date!(now - DAY);
    params.not_after = date!(now + SERVER_DAYS * DAY);
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_key = KeyPair::generate().map_err(err)?;
    let server_cert = params.signed_by(&server_key, &issuer).map_err(err)?;
    crate::auth::write_private(&key, &server_key.serialize_pem())?;
    std::fs::write(&cert, server_cert.pem())?;
    std::fs::write(&expires, (now + SERVER_DAYS * DAY).to_string())?;
    std::fs::write(&hosts_file, hosts.join(","))?;
    Ok(Generated {
        ca: ca_cert,
        cert,
        key,
        renewed: true,
    })
}

/// The CA's parameters. They are rebuilt identically each time, so the
/// stored CA key can sign renewals without parsing the stored CA certificate.
fn ca_params() -> std::io::Result<CertificateParams> {
    let mut params = CertificateParams::new(Vec::<String>::new())
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "bak-server private CA");
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let now = unix_now();
    params.not_before = date!(now - DAY);
    params.not_after = date!(now + CA_DAYS * DAY);
    Ok(params)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Year, month and day (UTC) of the day containing `secs` (civil-from-days, H. Hinnant).
fn ymd(secs: u64) -> (i32, u8, u8) {
    let z = (secs / DAY) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let y = (yoe + era * 400 + i64::from(m <= 2)) as i32;
    (y, m, d)
}

/// Build a rustls server config from PEM files.
pub fn server_config(cert: &Path, key: &Path) -> std::io::Result<Arc<rustls::ServerConfig>> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .map_err(|e| std::io::Error::other(format!("{}: {e}", cert.display())))?
        .collect::<Result<_, _>>()
        .map_err(|e| std::io::Error::other(format!("{}: {e}", cert.display())))?;
    let key = PrivateKeyDer::from_pem_file(key)
        .map_err(|e| std::io::Error::other(format!("{}: {e}", key.display())))?;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| std::io::Error::other(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        // 2026-10-11 00:00:00 UTC
        assert_eq!(ymd(1_791_676_800), (2026, 10, 11));
        assert_eq!(ymd(0), (1970, 1, 1));
        assert_eq!(ymd(951_782_400), (2000, 2, 29));
    }

    #[test]
    fn generates_and_reuses() {
        let dir = tempfile::tempdir().unwrap();
        let hosts = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        let g = ensure_generated(dir.path(), &hosts).unwrap();
        assert!(g.renewed);
        let ca = std::fs::read(&g.ca).unwrap();
        server_config(&g.cert, &g.key).unwrap();
        let again = ensure_generated(dir.path(), &hosts).unwrap();
        assert!(!again.renewed);
        // New host names renew the server certificate but keep the CA.
        let more = vec!["backup.lan".to_string()];
        assert!(ensure_generated(dir.path(), &more).unwrap().renewed);
        assert_eq!(std::fs::read(&g.ca).unwrap(), ca);
    }
}
