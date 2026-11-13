//! The server's certificate: self-signed, or issued through Tailscale.

use std::net::UdpSocket;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use rcgen::{CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, KeyPair, KeyUsagePurpose};
use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use time::{Duration, OffsetDateTime};

use crate::platform::{self, data_directory, write_private};

/// How often the certificates are looked at again: Tailscale's renewed when it is due, and the UDP side's own
/// made anew (see quic.rs).
pub const RENEWAL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// An address set aside for documentation (RFC 5737), which a UDP socket can be pointed at to learn the way
/// out without anything being sent.
const SOMEWHERE: &str = "192.0.2.1:9";

/// Apple's platforms refuse server certificates valid for more than 825 days.
const VALID_DAYS: i64 = 820;

pub fn path() -> PathBuf {
    data_directory().join("certificate.pem")
}

fn key_path() -> PathBuf {
    data_directory().join("key.pem")
}

/// Holds this machine's Tailscale name while the certificate comes from Tailscale; absent when it is self-signed.
fn tailscale_marker() -> PathBuf {
    data_directory().join("tailscale")
}

/// The name the Tailscale certificate is for, if that is the certificate in use.
pub fn tailscale_name() -> Option<String> {
    std::fs::read_to_string(tailscale_marker()).ok().map(|name| name.trim().to_string())
}

/// This machine's name on its tailnet, taken from `tailscale status --json`.
fn tailnet_name(status: &[u8]) -> Result<String, String> {
    let status: serde_json::Value =
        serde_json::from_slice(status).map_err(|_| "tailscale is not running or not logged in")?;
    let name = status["Self"]["DNSName"].as_str().unwrap_or("").trim_end_matches('.');
    let domains = status["CertDomains"].as_array().map(Vec::as_slice).unwrap_or_default();
    if name.is_empty() || !domains.iter().any(|domain| domain.as_str() == Some(name)) {
        return Err(
            "this tailnet has no HTTPS certificates; enable them in the Tailscale admin console under DNS".into()
        );
    }
    Ok(name.to_string())
}

/// Switches to a certificate issued through Tailscale, which browsers trust without any setup.
pub fn use_tailscale() -> Result<(), String> {
    let status = Command::new("tailscale")
        .args(["status", "--json"])
        .output()
        .map_err(|error| format!("cannot run tailscale: {error}"))?;
    let name = tailnet_name(&status.stdout)?;
    fetch_tailscale_certificate(&name)?;
    std::fs::write(tailscale_marker(), &name).map_err(|error| error.to_string())
}

/// Asks Tailscale for the certificate. It hands back the one it has unless that is due for renewal, so this is
/// also how the certificate stays current.
fn fetch_tailscale_certificate(name: &str) -> Result<(), String> {
    let output = Command::new("tailscale")
        .args(["cert", "--cert-file"])
        .arg(path())
        .arg("--key-file")
        .arg(key_path())
        .arg(name)
        .output()
        .map_err(|error| format!("cannot run tailscale: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "tailscale could not issue a certificate for {name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// The names and addresses clients may use for this machine, which the certificate has to list.
pub fn names() -> Vec<String> {
    let host = platform::hostname();
    let mut names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    if !host.is_empty() {
        names.extend([format!("{host}.local"), host]);
    }
    // The address this machine would use to reach others, found without sending anything.
    let address = UdpSocket::bind("0.0.0.0:0").and_then(|socket| {
        socket.connect(SOMEWHERE)?;
        socket.local_addr()
    });
    if let Ok(address) = address {
        names.push(address.ip().to_string());
    }
    names.reverse();
    names
}

/// What a certificate of this machine's names starts from: valid for `days`, from a day ago on, so that a client
/// whose clock is a little behind takes it too.
pub fn parameters(days: i64) -> Result<CertificateParams, String> {
    let mut parameters = CertificateParams::new(names()).map_err(|error| error.to_string())?;
    let now = OffsetDateTime::now_utc();
    parameters.not_before = now - Duration::days(1);
    parameters.not_after = now + Duration::days(days);
    Ok(parameters)
}

/// The TLS library set up with the cryptography used throughout.
pub fn tls_builder() -> rustls::ConfigBuilder<ServerConfig, rustls::WantsVersions> {
    ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
}

/// Creates a new self-signed certificate and key, replacing any existing ones.
pub fn create() -> Result<(), String> {
    let _ = std::fs::remove_file(tailscale_marker());
    let mut parameters = parameters(VALID_DAYS)?;
    parameters.distinguished_name = DistinguishedName::new();
    parameters.distinguished_name.push(DnType::CommonName, format!("Remote desktop on {}", names()[1]));
    parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let key = KeyPair::generate().map_err(|error| error.to_string())?;
    let certificate = parameters.self_signed(&key).map_err(|error| error.to_string())?;
    write_private(&key_path(), key.serialize_pem().as_bytes())?;
    std::fs::write(path(), certificate.pem()).map_err(|error| format!("cannot write {}: {error}", path().display()))
}

/// The server's TLS setup, creating the certificate on first use and renewing a Tailscale one when it is due.
pub fn configuration() -> Result<Arc<ServerConfig>, String> {
    if let Some(name) = tailscale_name() {
        if let Err(error) = fetch_tailscale_certificate(&name) {
            warn!("keeping the current certificate: {error}");
        }
    } else if !path().exists() || !key_path().exists() {
        create()?;
    }
    let unreadable = |error| format!("cannot read the certificate or its key: {error}");
    let chain = CertificateDer::pem_file_iter(path())
        .map_err(unreadable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unreadable)?;
    let key = PrivateKeyDer::from_pem_file(key_path()).map_err(unreadable)?;
    let configuration = tls_builder()
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|error| error.to_string())?;
    Ok(Arc::new(configuration))
}

#[cfg(test)]
mod tests {
    use super::tailnet_name;

    #[test]
    fn reads_the_tailnet_name() {
        let status = br#"{"Self": {"DNSName": "desk.example.ts.net."}, "CertDomains": ["desk.example.ts.net"]}"#;
        assert_eq!(tailnet_name(status).as_deref(), Ok("desk.example.ts.net"));
        let without_certificates = br#"{"Self": {"DNSName": "desk.example.ts.net."}, "CertDomains": null}"#;
        assert!(tailnet_name(without_certificates).unwrap_err().contains("HTTPS certificates"));
        assert!(tailnet_name(b"").unwrap_err().contains("not running"));
    }
}
