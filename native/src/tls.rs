//! TLS for the server: its certificate (self-signed, or issued through Tailscale), and a connection that one
//! thread can read while another writes.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, UdpSocket};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rcgen::{CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, KeyPair, KeyUsagePurpose};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection};
use time::{Duration, OffsetDateTime};

use crate::install::{config_directory, write_private};

/// Apple's platforms refuse server certificates valid for more than 825 days.
const VALID_DAYS: i64 = 820;
/// How much unsent data the kernel may hold for one client. Without a limit it buffers megabytes, which on a
/// slow link is seconds of stale video; with it the writer blocks and the capture skips frames instead.
const UNSENT_LIMIT: libc::c_int = 16 * 1024;

pub fn certificate_path() -> PathBuf {
    config_directory().join("remote-desktop/certificate.pem")
}

fn key_path() -> PathBuf {
    config_directory().join("remote-desktop/key.pem")
}

/// Holds this machine's Tailscale name while the certificate comes from Tailscale; absent when it is self-signed.
fn tailscale_marker() -> PathBuf {
    config_directory().join("remote-desktop/tailscale")
}

/// The name the Tailscale certificate is for, if that is the certificate in use.
pub fn tailscale_name() -> Option<String> {
    std::fs::read_to_string(tailscale_marker()).ok().map(|name| name.trim().to_string())
}

/// This machine's name on its tailnet, taken from `tailscale status --json`.
fn tailnet_name(status: &[u8]) -> Result<String, String> {
    let status: serde_json::Value = serde_json::from_slice(status).map_err(|_| "tailscale is not running or not logged in")?;
    let name = status["Self"]["DNSName"].as_str().unwrap_or("").trim_end_matches('.');
    let domains = status["CertDomains"].as_array().map(Vec::as_slice).unwrap_or_default();
    if name.is_empty() || !domains.iter().any(|domain| domain.as_str() == Some(name)) {
        return Err("this tailnet has no HTTPS certificates; enable them in the Tailscale admin console under DNS".into());
    }
    Ok(name.to_string())
}

/// Switches to a certificate issued through Tailscale, which browsers trust without any setup.
pub fn use_tailscale() -> Result<(), String> {
    let status = Command::new("tailscale").args(["status", "--json"]).output().map_err(|error| format!("cannot run tailscale: {error}"))?;
    let name = tailnet_name(&status.stdout)?;
    fetch_tailscale_certificate(&name)?;
    std::fs::write(tailscale_marker(), &name).map_err(|error| error.to_string())
}

/// Asks Tailscale for the certificate. It hands back the one it has unless that is due for renewal, so this is
/// also how the certificate stays current.
fn fetch_tailscale_certificate(name: &str) -> Result<(), String> {
    let output = Command::new("tailscale")
        .args(["cert", "--cert-file"])
        .arg(certificate_path())
        .arg("--key-file")
        .arg(key_path())
        .arg(name)
        .output()
        .map_err(|error| format!("cannot run tailscale: {error}"))?;
    if !output.status.success() {
        return Err(format!("tailscale could not issue a certificate for {name}: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(())
}

/// The names and addresses clients may use for this machine, which the certificate has to list.
pub fn names() -> Vec<String> {
    let mut buffer = [0u8; 256];
    let host = match unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len() - 1) } {
        0 => String::from_utf8_lossy(&buffer[..buffer.iter().position(|&byte| byte == 0).unwrap_or(0)]).into_owned(),
        _ => String::new(),
    };
    let mut names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    if !host.is_empty() {
        names.extend([format!("{host}.local"), host]);
    }
    // The address this machine would use to reach others, found without sending anything.
    let address = UdpSocket::bind("0.0.0.0:0").and_then(|socket| {
        socket.connect("192.0.2.1:9")?;
        socket.local_addr()
    });
    if let Ok(address) = address {
        names.push(address.ip().to_string());
    }
    names.reverse();
    names
}

/// Creates a new self-signed certificate and key, replacing any existing ones.
pub fn create_certificate() -> Result<(), String> {
    let _ = std::fs::remove_file(tailscale_marker());
    let names = names();
    let mut parameters = CertificateParams::new(names.clone()).map_err(|error| error.to_string())?;
    parameters.distinguished_name = DistinguishedName::new();
    parameters.distinguished_name.push(DnType::CommonName, format!("Remote desktop on {}", names[1]));
    let now = OffsetDateTime::now_utc();
    parameters.not_before = now - Duration::days(1);
    parameters.not_after = now + Duration::days(VALID_DAYS);
    parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let key = KeyPair::generate().map_err(|error| error.to_string())?;
    let certificate = parameters.self_signed(&key).map_err(|error| error.to_string())?;
    write_private(&key_path(), key.serialize_pem().as_bytes())?;
    std::fs::write(certificate_path(), certificate.pem()).map_err(|error| format!("cannot write {}: {error}", certificate_path().display()))
}

/// The server's TLS setup, creating the certificate on first use and renewing a Tailscale one when it is due.
pub fn configuration() -> Result<Arc<ServerConfig>, String> {
    if let Some(name) = tailscale_name() {
        if let Err(error) = fetch_tailscale_certificate(&name) {
            eprintln!("remote-desktop: keeping the current certificate: {error}");
        }
    } else if !certificate_path().exists() || !key_path().exists() {
        create_certificate()?;
    }
    let unreadable = |error| format!("cannot read the certificate or its key: {error}");
    let chain = CertificateDer::pem_file_iter(certificate_path()).map_err(unreadable)?.collect::<Result<Vec<_>, _>>().map_err(unreadable)?;
    let key = PrivateKeyDer::from_pem_file(key_path()).map_err(unreadable)?;
    let configuration = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|error| error.to_string())?;
    Ok(Arc::new(configuration))
}

/// One client's encrypted connection. A reader thread and a writer thread share it: the TLS state is locked only
/// while bytes are encrypted or decrypted, never while waiting on the network, so neither holds the other up.
pub struct Connection {
    tls: Mutex<ServerConnection>,
    socket: TcpStream,
    /// Set when reading produced a TLS-level reply that the writer still has to send.
    reply_pending: AtomicBool,
}

impl Connection {
    pub fn accept(configuration: &Arc<ServerConfig>, mut socket: TcpStream) -> io::Result<Self> {
        socket.set_nodelay(true)?;
        let limit = UNSENT_LIMIT;
        unsafe { libc::setsockopt(socket.as_raw_fd(), libc::IPPROTO_TCP, libc::TCP_NOTSENT_LOWAT, (&raw const limit).cast(), size_of::<libc::c_int>() as u32) };
        let mut tls = ServerConnection::new(configuration.clone()).map_err(io::Error::other)?;
        tls.set_buffer_limit(None); // `write` hands everything to the socket right away
        while tls.is_handshaking() {
            tls.complete_io(&mut socket)?;
        }
        while tls.wants_write() {
            tls.write_tls(&mut socket)?;
        }
        Ok(Self { tls: Mutex::new(tls), socket, reply_pending: AtomicBool::new(false) })
    }

    /// Encrypts and sends `bytes`. Only one thread may write at a time; an empty slice just sends pending replies.
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut wire = Vec::with_capacity(bytes.len() + 256);
        {
            let mut tls = self.tls.lock().unwrap();
            tls.writer().write_all(bytes)?;
            while tls.wants_write() {
                tls.write_tls(&mut wire)?;
            }
        }
        (&self.socket).write_all(&wire)
    }

    /// Whether a TLS-level reply is waiting for the writer; asking clears the flag.
    pub fn take_reply_pending(&self) -> bool {
        self.reply_pending.swap(false, Ordering::Relaxed)
    }

    pub fn close(&self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}

impl Read for &Connection {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.tls.lock().unwrap().reader().read(buffer) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                result => return result,
            }
            // Nothing decrypted is left: wait for the network without holding the lock the writer needs.
            let mut poll = libc::pollfd { fd: self.socket.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            unsafe { libc::poll(&mut poll, 1, -1) };
            let mut tls = self.tls.lock().unwrap();
            if tls.read_tls(&mut &self.socket)? == 0 {
                return Ok(0);
            }
            tls.process_new_packets().map_err(io::Error::other)?;
            if tls.wants_write() {
                self.reply_pending.store(true, Ordering::Relaxed);
            }
        }
    }
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
