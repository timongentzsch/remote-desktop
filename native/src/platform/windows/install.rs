//! Installing on Windows: the binary in the user's programs folder, started at logon by the user's Run key, and
//! a firewall rule that lets others reach it, which Windows asks an administrator to allow (see ../../install.rs
//! for the rest). Nothing of it needs an administrator but the rule.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use crate::install::{run, run_quietly};
use crate::{NAME, fail};

pub const BINARY: &str = r"%LOCALAPPDATA%\Programs\remote-desktop\remote-desktop.exe";
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
/// Tailscale's addresses, which a host with a certificate from Tailscale answers alone.
const TAILNET: &str = "100.64.0.0/10,fd7a:115c:a1e0::/48";
/// How the host is started: on its own, with no console window.
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn binary() -> PathBuf {
    let programs =
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| fail("LOCALAPPDATA is not set"));
    programs.join("Programs").join(NAME).join(format!("{NAME}.exe"))
}

/// Installing again restarts the host.
pub fn restart_command() -> String {
    format!("{NAME} install")
}

/// Copies this binary into place, starts it at every logon from now on, opens the firewall for it and starts it.
pub fn start(tailnet_only: bool) {
    let this = std::env::current_exe().unwrap_or_else(|error| fail(&format!("cannot locate this program: {error}")));
    let binary = binary();
    end_running();
    if this != binary {
        std::fs::create_dir_all(binary.parent().unwrap())
            .and_then(|()| std::fs::copy(&this, &binary))
            .unwrap_or_else(|error| fail(&format!("cannot copy the program to {}: {error}", binary.display())));
    }
    let command = format!("\"{}\"", binary.display());
    run("reg", &["add", RUN_KEY, "/v", NAME, "/t", "REG_SZ", "/d", &command, "/f"]);
    open_firewall(tailnet_only);
    // The host outlives this program, and Windows leaves nothing behind of a process nobody waits for.
    #[allow(clippy::zombie_processes)]
    Command::new(&binary)
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()
        .unwrap_or_else(|error| fail(&format!("cannot start {}: {error}", binary.display())));
    println!("Installed {} and started it; from now on it starts at logon.", binary.display());
}

/// Undoes `start`.
pub fn stop() {
    end_running();
    run_quietly("reg", &["delete", RUN_KEY, "/v", NAME, "/f"]);
    as_administrator(&format!("netsh advfirewall firewall delete rule name={NAME}"));
    let _ = std::fs::remove_file(binary());
}

/// The rule follows where the certificate comes from: everyone, or the tailnet only.
pub fn follow_certificate(tailnet_only: bool) {
    open_firewall(tailnet_only);
}

/// Tailscale on Windows lets its user have certificates.
pub fn allow_tailscale() {}

/// Ends a host that runs, other than this program.
fn end_running() {
    let others = format!("PID ne {}", std::process::id());
    run_quietly("taskkill", &["/F", "/FI", &others, "/IM", &format!("{NAME}.exe")]);
}

/// Lets others reach the program, over TCP and UDP alike: the one rule for it, made anew.
fn open_firewall(tailnet_only: bool) {
    let from = if tailnet_only { format!(" remoteip={TAILNET}") } else { String::new() };
    let rule = format!(
        "netsh advfirewall firewall delete rule name={NAME} & netsh advfirewall firewall add rule name={NAME} \
         dir=in action=allow enable=yes profile=any program=\"{}\"{from}",
        binary().display()
    );
    if !as_administrator(&rule) {
        println!("The firewall was left as it is: until it lets {NAME} in, only this computer can reach it.");
    }
}

/// Runs `commands` in cmd as an administrator, which Windows asks the user to allow. False if they did not.
fn as_administrator(commands: &str) -> bool {
    let quoted = commands.replace('\'', "''");
    let elevated = format!(
        "$p = Start-Process cmd -ArgumentList '/c {quoted}' -Verb RunAs -Wait -WindowStyle Hidden -PassThru; exit $p.ExitCode"
    );
    run_quietly("powershell", &["-NoProfile", "-NonInteractive", "-Command", &elevated])
}
