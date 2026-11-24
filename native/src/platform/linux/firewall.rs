//! The firewall, where one is active that this knows how to configure: ufw or firewalld. The host's port is
//! opened for TCP, which the page and the session take, and for UDP of the same number, which video takes where
//! it can (see quic.rs). With a certificate from Tailscale it is opened on Tailscale's interface only, as the
//! host then answers nobody else (see `server::on_tailnet`).

use crate::install::{run, run_quietly};
use crate::{NAME, PORT};

/// Tailscale's network interface, the one way in for devices on the tailnet.
const TAILNET_INTERFACE: &str = "tailscale0";
const UFW_PROFILE: &str = "/etc/ufw/applications.d/remote-desktop";
const FIREWALLD_PROFILE: &str = "/etc/firewalld/services/remote-desktop.xml";

#[derive(Clone, Copy)]
enum Firewall {
    Ufw,
    Firewalld,
}

impl Firewall {
    fn active() -> Option<Self> {
        if sudo_output(&["ufw", "status"]).is_some_and(|status| status.contains("Status: active")) {
            Some(Self::Ufw)
        } else if sudo_output(&["firewall-cmd", "--state"]).as_deref() == Some("running") {
            Some(Self::Firewalld)
        } else {
            None
        }
    }

    /// A dedicated service lets uninstall remove our rules even when another service also allows HTTPS.
    fn profile(self) -> (&'static str, String) {
        match self {
            Self::Ufw => (
                UFW_PROFILE,
                format!(
                    "[{NAME}]\ntitle=Remote desktop\ndescription=Remote desktop over HTTPS and QUIC\nports={PORT}/tcp|{PORT}/udp"
                ),
            ),
            Self::Firewalld => (
                FIREWALLD_PROFILE,
                format!(
                    r#"<service><port port="{PORT}" protocol="tcp"/><port port="{PORT}" protocol="udp"/></service>"#
                ),
            ),
        }
    }

    fn change(self, add: bool, tailnet_only: bool) {
        let (path, profile) = self.profile();
        if add {
            run("sudo", &["sh", "-c", "printf '%s\\n' \"$1\" > \"$2\"", "sh", &profile, path]);
        } else if !std::path::Path::new(path).exists() {
            return;
        }
        match self {
            Self::Ufw => {
                let all = ["allow", NAME];
                let tailnet = ["allow", "in", "on", TAILNET_INTERFACE, "to", "any", "app", NAME];
                for rule in [&all[..], &tailnet[..]] {
                    run_quietly("sudo", &[&["ufw", "delete"][..], rule].concat());
                }
                if add {
                    let rule = if tailnet_only { &tailnet[..] } else { &all[..] };
                    run("sudo", &[&["ufw"][..], rule].concat());
                }
            }
            Self::Firewalld => {
                // Reload once to discover a new service definition. Clear just our service from installed zones.
                if add {
                    run("sudo", &["firewall-cmd", "--quiet", "--reload"]);
                }
                let zones = sudo_output(&["firewall-cmd", "--permanent", "--get-zones"])
                    .unwrap_or_else(|| crate::fail("cannot list firewall zones"));
                let remove = format!("--remove-service={NAME}");
                for zone in zones.split_whitespace() {
                    run_quietly("sudo", &["firewall-cmd", "--permanent", &format!("--zone={zone}"), &remove]);
                }
                if add {
                    let query = if tailnet_only {
                        format!("--get-zone-of-interface={TAILNET_INTERFACE}")
                    } else {
                        "--get-default-zone".to_owned()
                    };
                    if let Some(zone) = sudo_output(&["firewall-cmd", &query]).filter(|zone| !zone.is_empty()) {
                        let zone = format!("--zone={zone}");
                        run("sudo", &["firewall-cmd", "--permanent", &zone, &format!("--add-service={NAME}")]);
                    } else {
                        println!("No matching firewalld zone. Assign the interface a zone to enable access.");
                    }
                }
            }
        }
        if !add {
            run("sudo", &["rm", "-f", path]);
        }
        if matches!(self, Self::Firewalld) {
            run("sudo", &["firewall-cmd", "--quiet", "--reload"]);
        }
    }
}

/// Opens the service to everyone or only to Tailscale.
pub fn open(tailnet_only: bool) {
    if let Some(firewall) = Firewall::active() {
        firewall.change(true, tailnet_only);
    }
}

/// Before an install changes the password, certificate or old binary, check that the host can take TCP.
/// Opening firewall rules for UDP must also leave an unrelated service's exposure alone.
pub fn check_port() {
    check_listeners(Firewall::active().is_some());
}

pub fn follow_certificate(tailnet_only: bool) {
    if let Some(firewall) = Firewall::active() {
        check_listeners(true);
        firewall.change(true, tailnet_only);
    }
}

fn check_listeners(udp: bool) {
    // The kernel's socket tables need no extra packages or privileges. Ask ss for owners only if
    // something is listening, or if the tables are unavailable. IPv6 can be disabled in the kernel.
    let port = format!(":{PORT:04X}");
    let free = |table: &str| match std::fs::read_to_string(format!("/proc/net/{table}")) {
        Ok(rows) => !rows.lines().skip(1).any(|row| {
            let mut fields = row.split_whitespace();
            fields.nth(1).is_some_and(|address| address.ends_with(&port))
                && (table.starts_with("udp") || fields.nth(1) == Some("0A")) // TCP_LISTEN
        }),
        Err(error) => table.ends_with('6') && error.kind() == std::io::ErrorKind::NotFound,
    };
    if free("tcp") && free("tcp6") && (!udp || free("udp") && free("udp6")) {
        return;
    }
    let flags = if udp { "-Hlnptu" } else { "-Hlnpt" };
    let sockets = sudo_output(&["ss", flags, &format!("sport = :{PORT}")])
        .unwrap_or_else(|| crate::fail(&format!("cannot identify the owner of port {PORT}; install iproute2 (ss)")));
    let installed = |pid: u32| {
        let executable = format!("/proc/{pid}/exe");
        run_quietly("sudo", &["test", &executable, "-ef", super::install::BINARY])
    };
    if sockets.lines().any(|line| !host_owns_socket(line, installed)) {
        crate::fail(&format!("port {PORT} is in use by another service. Choose another port with --listen ADDRESS."));
    }
}

fn host_owns_socket(line: &str, installed: impl Fn(u32) -> bool) -> bool {
    // ss lists each socket owner as ("command",pid=...,fd=...). Reject a socket with unknown ownership.
    let mut owners = line.split("(\"").skip(1).peekable();
    owners.peek().is_some()
        && owners.all(|owner| {
            let pid = owner.split_once("\",pid=").and_then(|(_, rest)| rest.split_once(','));
            pid.and_then(|(number, _)| number.parse().ok()).is_some_and(&installed)
        })
}

pub fn close() {
    if let Some(firewall) = Firewall::active() {
        firewall.change(false, false);
    }
}

fn sudo_output(arguments: &[&str]) -> Option<String> {
    let output = std::process::Command::new("sudo").args(arguments).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_existing_listener_must_belong_to_this_host() {
        let installed = |pid| pid == 23;
        assert!(host_owns_socket(r#"users:(("remote-desktop",pid=23,fd=8))"#, installed));
        assert!(!host_owns_socket("", installed));
        assert!(!host_owns_socket(
            r#"users:(("remote-desktop",pid=23,fd=8),("remote-desktop",pid=22,fd=3))"#,
            installed
        ));
    }

    #[test]
    fn both_firewalls_identify_our_service_separately_from_https() {
        let (_, ufw) = Firewall::Ufw.profile();
        assert!(ufw.starts_with("[remote-desktop]\n"));
        assert!(ufw.contains("ports=443/tcp|443/udp"));
        let (path, xml) = Firewall::Firewalld.profile();
        assert!(path.ends_with("/remote-desktop.xml"));
        for protocol in ["tcp", "udp"] {
            assert!(xml.contains(&format!(r#"<port port="443" protocol="{protocol}"/>"#)));
        }
    }
}
