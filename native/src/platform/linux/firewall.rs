//! The firewall, where one is active that this knows how to configure: ufw or firewalld. The host's port is
//! opened for TCP, which the page and the session take, and for UDP of the same number, which video takes where
//! it can (see quic.rs). With a certificate from Tailscale it is opened for the tailnet only, as the host then
//! answers nobody else (see `server::on_tailnet`): on Tailscale's interface, or, where firewalld has that
//! interface in no zone of its own, for the tailnet's addresses.

use crate::install::{run, run_quietly};
use crate::{NAME, port};

/// Tailscale's network interface, the one way in for devices on the tailnet.
const TAILNET_INTERFACE: &str = "tailscale0";
const UFW_PROFILE: &str = "/etc/ufw/applications.d/remote-desktop";
const FIREWALLD_PROFILE: &str = "/etc/firewalld/services/remote-desktop.xml";
/// The port of versions before 0.10, whose rules for it an install takes away.
const OLD_PORT: &str = "8765";

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
    fn profile(self, port: u16) -> (&'static str, String) {
        match self {
            Self::Ufw => (
                UFW_PROFILE,
                format!(
                    "[{NAME}]\ntitle=Remote desktop\ndescription=Remote desktop over HTTPS and QUIC\nports={port}/tcp|{port}/udp"
                ),
            ),
            Self::Firewalld => (
                FIREWALLD_PROFILE,
                format!(
                    r#"<service><port port="{port}" protocol="tcp"/><port port="{port}" protocol="udp"/></service>"#
                ),
            ),
        }
    }

    fn change(self, add: bool, tailnet_only: bool) {
        let (path, profile) = self.profile(port());
        if !add && !std::path::Path::new(path).exists() {
            return;
        }
        let write = || run("sudo", &["sh", "-c", "printf '%s\\n' \"$1\" > \"$2\"", "sh", &profile, path]);
        match self {
            Self::Ufw => {
                let all = ["allow", NAME];
                let tailnet = ["allow", "in", "on", TAILNET_INTERFACE, "to", "any", "app", NAME];
                // ufw finds a rule to delete by the ports its profile has: the old profile has to be there still.
                for rule in [&all[..], &tailnet[..]] {
                    run_quietly("sudo", &[&["ufw", "delete"][..], rule].concat());
                }
                for protocol in ["tcp", "udp"] {
                    let tailnet = ["in", "on", TAILNET_INTERFACE, "to", "any", "port", OLD_PORT, "proto", protocol];
                    run_quietly("sudo", &["ufw", "delete", "allow", &format!("{OLD_PORT}/{protocol}")]);
                    run_quietly("sudo", &[&["ufw", "delete", "allow"][..], &tailnet].concat());
                }
                if add {
                    write();
                    let rule = if tailnet_only { &tailnet[..] } else { &all[..] };
                    run("sudo", &[&["ufw"][..], rule].concat());
                }
            }
            Self::Firewalld => {
                // Reload once to discover a new service definition. Clear just our service from installed zones.
                if add {
                    write();
                    run("sudo", &["firewall-cmd", "--quiet", "--reload"]);
                }
                let zones = sudo_output(&["firewall-cmd", "--permanent", "--get-zones"])
                    .unwrap_or_else(|| crate::fail("cannot list firewall zones"));
                let remove = format!("--remove-service={NAME}");
                let old = ["tcp", "udp"].map(|protocol| format!("--remove-port={OLD_PORT}/{protocol}"));
                let tailnet = tailnet_rules();
                for zone in zones.split_whitespace() {
                    let zone = format!("--zone={zone}");
                    // One kind of change a call: a service that is not there ends the call before the ports.
                    run_quietly("sudo", &["firewall-cmd", "--permanent", &zone, &remove]);
                    run_quietly("sudo", &["firewall-cmd", "--permanent", &zone, &old[0], &old[1]]);
                    for rule in &tailnet {
                        run_quietly(
                            "sudo",
                            &["firewall-cmd", "--permanent", &zone, &format!("--remove-rich-rule={rule}")],
                        );
                    }
                }
                if add {
                    let zone_of = |query: &str| sudo_output(&["firewall-cmd", query]).filter(|zone| !zone.is_empty());
                    let interface = format!("--get-zone-of-interface={TAILNET_INTERFACE}");
                    let own = tailnet_only.then(|| zone_of(&interface)).flatten();
                    let service = format!("--add-service={NAME}");
                    if let Some(zone) = own {
                        run("sudo", &["firewall-cmd", "--permanent", &format!("--zone={zone}"), &service]);
                    } else if let Some(zone) = zone_of("--get-default-zone") {
                        // Tailscale's interface is in no zone unless someone put it in one: the default zone
                        // then speaks for it, and there the tailnet is let in by its addresses.
                        let zone = format!("--zone={zone}");
                        let rules = tailnet.iter().map(|rule| format!("--add-rich-rule={rule}"));
                        for change in if tailnet_only { rules.collect() } else { vec![service] } {
                            run("sudo", &["firewall-cmd", "--permanent", &zone, &change]);
                        }
                    } else {
                        println!("firewalld names no default zone: the port is left closed.");
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
pub fn check_port(port: u16) {
    check_listeners(port, Firewall::active().is_some());
}

pub fn follow_certificate(tailnet_only: bool) {
    if let Some(firewall) = Firewall::active() {
        check_listeners(port(), true);
        firewall.change(true, tailnet_only);
    }
}

fn check_listeners(number: u16, udp: bool) {
    // The kernel's socket tables need no extra packages or privileges. Ask ss for owners only if
    // something is listening, or if the tables are unavailable. IPv6 can be disabled in the kernel.
    let port = format!(":{number:04X}");
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
    let sockets = sudo_output(&["ss", flags, &format!("sport = :{number}")])
        .unwrap_or_else(|| crate::fail(&format!("cannot identify the owner of port {number}; install iproute2 (ss)")));
    let installed = |pid: u32| {
        let executable = format!("/proc/{pid}/exe");
        run_quietly("sudo", &["test", &executable, "-ef", super::install::BINARY])
    };
    if sockets.lines().any(|line| !host_owns_socket(line, installed)) {
        crate::fail(&format!("port {number} is in use by another service. Install on another with --port=PORT."));
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

/// firewalld's rules that let the tailnet's addresses, and nobody else, reach the service.
fn tailnet_rules() -> [String; 2] {
    let (v4, v6) = (crate::server::TAILNET_V4, crate::server::TAILNET_V6);
    [("ipv4", format!("{}/{}", v4.0, v4.1)), ("ipv6", format!("{}/{}", v6.0, v6.1))].map(|(family, range)| {
        format!(r#"rule family="{family}" source address="{range}" service name="{NAME}" accept"#)
    })
}

/// The one language every tool speaks, whatever the system's is: the locale, and the list of languages that
/// programs in Python, as ufw and firewall-cmd are, put before it.
const PLAIN: [&str; 2] = ["LC_ALL=C", "LANGUAGE=C"];

/// What a tool says when run as root. It is read here, so the tool is asked in that language.
fn sudo_output(arguments: &[&str]) -> Option<String> {
    let output = std::process::Command::new("sudo").arg("env").args(PLAIN).args(arguments).output().ok()?;
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
        let (_, ufw) = Firewall::Ufw.profile(443);
        assert!(ufw.starts_with("[remote-desktop]\n"));
        assert!(ufw.contains("ports=443/tcp|443/udp"));
        let (path, xml) = Firewall::Firewalld.profile(443);
        assert!(path.ends_with("/remote-desktop.xml"));
        for protocol in ["tcp", "udp"] {
            assert!(xml.contains(&format!(r#"<port port="443" protocol="{protocol}"/>"#)));
        }
    }
}
