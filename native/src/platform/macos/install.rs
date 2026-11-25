//! Installing on macOS: the binary in /usr/local/bin and a LaunchAgent that runs it in the user's session from
//! login on (see ../../install.rs for the rest). macOS asks once for the permissions the host needs.

use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use super::input;
use crate::install::{check_tcp_port, run, run_quietly, serves, write};
use crate::{NAME, fail};

pub const BINARY: &str = "/usr/local/bin/remote-desktop";
const LABEL: &str = NAME;
/// How long the host takes to start where it is allowed, how long `install` waits for the user to allow it, and
/// how often it looks.
const STARTING: Duration = Duration::from_secs(2);
const LONGEST_WAIT: Duration = Duration::from_secs(600);
const CHECK: Duration = Duration::from_secs(1);

/// Runs the host at login and again whenever it ends; what it says goes to ~/Library/Logs.
fn agent() -> String {
    let log = home().join("Library/Logs").join(format!("{NAME}.log"));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{LABEL}</string>
    <key>ProgramArguments</key><array><string>{BINARY}</string></array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>ProcessType</key><string>Interactive</string>
    <key>StandardErrorPath</key><string>{}</string>
</dict>
</plist>
"#,
        log.display()
    )
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| fail("HOME is not set"))
}

fn agent_path() -> PathBuf {
    home().join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

/// The user's session, which launchd runs agents in, and the agent in it.
fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

fn service() -> String {
    format!("{}/{LABEL}", domain())
}

pub fn restart_command() -> String {
    format!("launchctl kickstart -k {}", service())
}

pub fn binary() -> PathBuf {
    BINARY.into()
}

/// Whether newer versions can install themselves, or else why not.
pub fn updates_itself() -> Result<(), &'static str> {
    Err("a Mac asks at the Mac for a new version's permissions. The page says when there is \
         one, and `remote-desktop update` installs it.")
}

/// Only the active LaunchAgent that this installer replaces may already hold the listening port.
pub fn check_port(port: u16) {
    check_tcp_port(port, || {
        let Ok(agent) = Command::new("/bin/launchctl").args(["print", &service()]).output() else { return false };
        let pid = String::from_utf8_lossy(&agent.stdout)
            .lines()
            .find_map(|line| line.strip_prefix("\tpid = ")?.parse::<u32>().ok());
        agent.status.success() && pid.is_some_and(|pid| listens_on(pid, port))
    })
    .unwrap_or_else(|error| fail(&error));
}

fn listens_on(pid: u32, port: u16) -> bool {
    let output = Command::new("/usr/sbin/lsof")
        .args(["-nP", "-a", "-p", &pid.to_string(), &format!("-i4TCP:{port}"), "-sTCP:LISTEN", "-Fn"])
        .output();
    let wildcard = format!("n*:{port}");
    output.is_ok_and(|result| {
        result.status.success() && String::from_utf8_lossy(&result.stdout).lines().any(|line| line == wildcard)
    })
}

/// Copies this binary into place (through sudo) and starts the host as the user's LaunchAgent.
pub fn start(_tailnet_only: bool) {
    let this = std::env::current_exe().unwrap_or_else(|error| fail(&format!("cannot locate this program: {error}")));
    run("sudo", &["mkdir", "-p", "/usr/local/bin"]);
    run("sudo", &["install", "-m755", &this.to_string_lossy(), BINARY]);
    write(&agent_path(), &agent());
    run_quietly("launchctl", &["bootout", &service()]);
    run("launchctl", &["bootstrap", &domain(), &agent_path().to_string_lossy()]);
    println!("Installed {BINARY} and started it; from now on it starts at login.");
    wait_until_serving();
}

/// Waits for the host to answer, which it does once macOS lets it record the screen and control the Mac.
fn wait_until_serving() {
    let port = crate::port();
    let serving = || {
        if TcpStream::connect(("127.0.0.1", port)).is_err() {
            return false;
        }
        if !serves(port) {
            fail(&format!(
                "port {port} is answering but did not serve the remote desktop client. Check for another HTTPS service."
            ));
        }
        true
    };
    let since = Instant::now();
    std::thread::sleep(STARTING);
    if !serving() {
        println!("On the Mac, remote-desktop is {}.", input::WAITING);
    }
    while !serving() {
        if since.elapsed() > LONGEST_WAIT {
            println!("Not allowed yet; the host starts by itself once it is.");
            return;
        }
        std::thread::sleep(CHECK);
    }
    println!("It is allowed and running.");
}

/// Undoes `start`.
pub fn stop() {
    run_quietly("launchctl", &["bootout", &service()]);
    let _ = std::fs::remove_file(agent_path());
    run("sudo", &["rm", "-f", BINARY]);
}

/// The host itself answers only the tailnet where the certificate is Tailscale's: there is no firewall to follow.
pub fn follow_certificate(_tailnet_only: bool) {}

/// Tailscale on a Mac lets its user have certificates.
pub fn allow_tailscale() {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn only_the_managed_process_can_replace_a_wildcard_listener() {
        let listener = TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(check_tcp_port(port, || listens_on(std::process::id(), port)).is_ok());
        assert!(check_tcp_port(port, || listens_on(1, port)).is_err());
        drop(listener);
        assert!(check_tcp_port(port, || panic!("free ports do not need an ownership lookup")).is_ok());
    }
}
