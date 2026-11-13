//! The firewall, where one is active that this knows how to configure: ufw or firewalld. The host's port is
//! opened for TCP, which the page and the session take, and for UDP of the same number, which video takes where
//! it can (see quic.rs). With a certificate from Tailscale it is opened on Tailscale's interface only, as the
//! host then answers nobody else (see `server::on_tailnet`).

use crate::PORT;
use crate::install::{run, run_quietly};

/// Tailscale's network interface, the one way in for devices on the tailnet.
const TAILNET_INTERFACE: &str = "tailscale0";
const PROTOCOLS: [&str; 2] = ["tcp", "udp"];

#[derive(Clone, Copy)]
enum Firewall {
    Ufw,
    Firewalld,
}

impl Firewall {
    /// The one that is active, if any.
    fn active() -> Option<Self> {
        let says = |program: &str, argument: &str, active: &str| {
            std::process::Command::new("sudo")
                .args([program, argument])
                .output()
                .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains(active))
        };
        if says("ufw", "status", "Status: active") {
            Some(Self::Ufw)
        } else if says("firewall-cmd", "--state", "running") {
            Some(Self::Firewalld)
        } else {
            None
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Ufw => "ufw",
            Self::Firewalld => "firewalld",
        }
    }
}

/// Opens the port, for everyone or only on Tailscale's interface. Whatever an earlier install opened is closed
/// first, so that switching between the two leaves one rule.
pub fn open(tailnet_only: bool) {
    let Some(firewall) = Firewall::active() else { return };
    close_in(firewall);
    match firewall {
        Firewall::Ufw => {
            for rule in ufw_rules(tailnet_only) {
                run("sudo", &[&["ufw", "allow"][..], &as_strs(&rule)].concat());
            }
        }
        Firewall::Firewalld => {
            let zone = match tailnet_only.then(tailnet_zone) {
                Some(None) => {
                    println!("firewalld puts {TAILNET_INTERFACE} in no zone, so the port stays closed. To open it for");
                    println!(
                        "the tailnet: sudo firewall-cmd --permanent --zone=trusted --add-interface={TAILNET_INTERFACE}"
                    );
                    return;
                }
                zone => zone.flatten(),
            };
            let ports = firewalld_ports("add", zone.as_deref());
            run("sudo", &[&["firewall-cmd", "--quiet", "--permanent"][..], &as_strs(&ports)].concat());
            run("sudo", &["firewall-cmd", "--quiet", "--reload"]);
        }
    }
    let scope = if tailnet_only { format!(" on {TAILNET_INTERFACE} only") } else { String::new() };
    println!("Opened port {PORT} (TCP and UDP){scope} in the firewall ({}).", firewall.name());
}

/// Removes whichever of the rules `open` makes are there.
pub fn close() {
    if let Some(firewall) = Firewall::active() {
        close_in(firewall);
    }
}

/// What is not there to remove is fine.
fn close_in(firewall: Firewall) {
    match firewall {
        Firewall::Ufw => {
            for rule in ufw_rules(false).into_iter().chain(ufw_rules(true)) {
                run_quietly("sudo", &[&["ufw", "delete", "allow"][..], &as_strs(&rule)].concat());
            }
        }
        Firewall::Firewalld => {
            for zone in [None, tailnet_zone()] {
                for port in firewalld_ports("remove", zone.as_deref()) {
                    run_quietly("sudo", &["firewall-cmd", "--quiet", "--permanent", &port]);
                }
            }
            run_quietly("sudo", &["firewall-cmd", "--quiet", "--reload"]);
        }
    }
}

/// ufw's rule for each protocol, to follow `allow` or `delete allow`.
fn ufw_rules(tailnet_only: bool) -> Vec<Vec<String>> {
    let port = PORT.to_string();
    let rule = |protocol: &str| match tailnet_only {
        true => {
            ["in", "on", TAILNET_INTERFACE, "to", "any", "port", &port, "proto", protocol].map(String::from).to_vec()
        }
        false => vec![format!("{port}/{protocol}")],
    };
    PROTOCOLS.map(rule).to_vec()
}

/// The zone firewalld put Tailscale's interface in, if any.
fn tailnet_zone() -> Option<String> {
    let output = std::process::Command::new("sudo")
        .args(["firewall-cmd", &format!("--get-zone-of-interface={TAILNET_INTERFACE}")])
        .output()
        .ok()?;
    let zone = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !zone.is_empty()).then_some(zone)
}

/// firewalld's arguments that `add` or `remove` both ports, in `zone` or else the default one.
fn firewalld_ports(change: &str, zone: Option<&str>) -> Vec<String> {
    let zone = zone.map(|zone| format!("--zone={zone}"));
    zone.into_iter().chain(PROTOCOLS.map(|protocol| format!("--{change}-port={PORT}/{protocol}"))).collect()
}

fn as_strs(strings: &[String]) -> Vec<&str> {
    strings.iter().map(String::as_str).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_for_everyone_or_the_tailnet() {
        assert_eq!(ufw_rules(false), [["8765/tcp"], ["8765/udp"]]);
        assert_eq!(ufw_rules(true)[1].join(" "), "in on tailscale0 to any port 8765 proto udp");
        assert_eq!(
            firewalld_ports("add", Some("trusted")),
            ["--zone=trusted", "--add-port=8765/tcp", "--add-port=8765/udp"]
        );
        assert_eq!(firewalld_ports("remove", None), ["--remove-port=8765/tcp", "--remove-port=8765/udp"]);
    }
}
