//! Installing on Windows: the binary in the user's programs folder, started at logon by the user's Run key, and
//! a firewall rule that lets others reach it, which Windows asks an administrator to allow (see ../../install.rs
//! for the rest). Nothing of it needs an administrator but the rule.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::Win32::Foundation::{HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation};
use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};

use crate::install::{check_tcp_port, run, run_quietly};
use crate::{NAME, fail};

pub const BINARY: &str = r"%LOCALAPPDATA%\Programs\remote-desktop\remote-desktop.exe";
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
/// Tailscale's addresses, which a host with a certificate from Tailscale answers alone.
const TAILNET: &str = "100.64.0.0/10,fd7a:115c:a1e0::/48";
/// How the host is started: on its own, with no console window.
const DETACHED_PROCESS: u32 = 0x0000_0008;
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn binary() -> PathBuf {
    let programs =
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| fail("LOCALAPPDATA is not set"));
    programs.join("Programs").join(NAME).join(format!("{NAME}.exe"))
}

/// Newer versions can install themselves: the binary is in the user's own folder.
pub fn updates_itself() -> Result<(), &'static str> {
    Ok(())
}

/// Starts the installed program as `remote-desktop update`, which outlives the host that it replaces. It has
/// a console without a window, and what it runs shares that.
pub fn ask_for_update() {
    if let Err(error) = Command::new(binary()).arg("update").creation_flags(CREATE_NO_WINDOW).spawn() {
        warn!("cannot start the update: {error}");
    }
}

/// The host leaves before the binary changes, and is started from the new one. Windows lets a running
/// program's file be renamed, which is how `update`, run from that file, makes room for the new one.
pub fn stop_for_update() {
    end_running(&binary());
}

pub fn start_updated() {
    let _ = start_host(&binary());
}

/// The host outlives this program, and Windows leaves nothing behind of a process nobody waits for. It takes
/// none of this program's input and output along: a new process gets every handle of the one that starts it
/// that may be handed on, and where those are pipes, as under ssh, whoever waits for them to close would wait
/// for the host.
#[allow(clippy::zombie_processes)]
fn start_host(binary: &Path) -> std::io::Result<()> {
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        if let Ok(handle) = unsafe { GetStdHandle(which) } {
            let _ = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) };
        }
    }
    let nothing = std::process::Stdio::null;
    Command::new(binary)
        .stdin(nothing())
        .stdout(nothing())
        .stderr(nothing())
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()
        .map(drop)
}

/// Installing again restarts the host.
pub fn restart_command() -> String {
    format!("{NAME} install")
}

/// The installed program may already be serving. A separate copy with the same name is a different host.
pub fn check_port(port: u16) {
    check_tcp_port(port, || listens_on(&binary(), port)).unwrap_or_else(|error| fail(&error));
}

fn listens_on(program: &Path, port: u16) -> bool {
    managed_command(program,
        "$listeners = @(Get-NetTCPConnection -LocalAddress 0.0.0.0 -LocalPort $env:RD_INSTALL_PORT -State Listen -ErrorAction Stop); \
         if (!$listeners.Count) { exit 1 }; foreach ($listener in $listeners) { \
         if ((Get-Process -Id $listener.OwningProcess -ErrorAction Stop).Path -ne $env:RD_INSTALL_PROGRAM) { exit 1 } }")
        .env("RD_INSTALL_PORT", port.to_string()).output().is_ok_and(|out| out.status.success())
}

/// Copies this binary into place, starts it at every logon from now on, opens the firewall for it and starts it.
pub fn start(tailnet_only: bool) {
    let this = std::env::current_exe().unwrap_or_else(|error| fail(&format!("cannot locate this program: {error}")));
    let binary = binary();
    end_running(&binary);
    if this != binary {
        std::fs::create_dir_all(binary.parent().unwrap())
            .and_then(|()| std::fs::copy(&this, &binary))
            .unwrap_or_else(|error| fail(&format!("cannot copy the program to {}: {error}", binary.display())));
    }
    let command = format!("\"{}\"", binary.display());
    run("reg", &["add", RUN_KEY, "/v", NAME, "/t", "REG_SZ", "/d", &command, "/f"]);
    open_firewall(tailnet_only);
    start_host(&binary).unwrap_or_else(|error| fail(&format!("cannot start {}: {error}", binary.display())));
    println!("Installed {} and started it; from now on it starts at logon.", binary.display());
}

/// Undoes `start`.
pub fn stop() {
    end_running(&binary());
    run_quietly("reg", &["delete", RUN_KEY, "/v", NAME, "/f"]);
    as_administrator(&format!("netsh advfirewall firewall delete rule name={NAME}"));
    let _ = std::fs::remove_file(binary());
    let _ = std::fs::remove_file(binary().with_extension("old")); // what an update leaves (see update.rs)
}

/// The rule follows where the certificate comes from: everyone, or the tailnet only.
pub fn follow_certificate(tailnet_only: bool) {
    open_firewall(tailnet_only);
}

/// Tailscale on Windows lets its user have certificates.
pub fn allow_tailscale() {}

/// Path values travel as environment data so quoting and special characters in user names are preserved.
fn managed_command(program: &Path, script: &str) -> Command {
    let mut command = Command::new("powershell");
    command.args(["-NoProfile", "-NonInteractive", "-Command", script]).env("RD_INSTALL_PROGRAM", program);
    command
}

/// Wait for hosts running this user's installed executable to release it before replacing the file.
fn end_running(program: &Path) {
    let status = managed_command(program,
        "$ErrorActionPreference = 'Stop'; \
         foreach ($p in (Get-Process -Name ([IO.Path]::GetFileNameWithoutExtension($env:RD_INSTALL_PROGRAM)) -ErrorAction SilentlyContinue)) { \
         if ($p.Id -ne $env:RD_INSTALL_PID -and $p.Path -eq $env:RD_INSTALL_PROGRAM) { \
         try { $p.Kill() } catch { if (!$p.HasExited) { throw } }; \
         if (!$p.WaitForExit(5000)) { throw 'Timed out stopping the installed host' } } }; exit 0")
        .env("RD_INSTALL_PID", std::process::id().to_string()).status();
    if !status.is_ok_and(|status| status.success()) {
        fail("cannot stop the installed host");
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::process::Stdio;

    #[test]
    fn only_the_installed_path_can_replace_a_wildcard_listener() {
        let listener = TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let program = std::env::current_exe().unwrap();
        assert!(check_tcp_port(port, || listens_on(&program, port)).is_ok());
        let other = program.with_file_name("other-host.exe");
        assert!(check_tcp_port(port, || listens_on(&other, port)).is_err());
    }

    #[test]
    fn stopping_the_installed_copy_leaves_a_same_named_program_running() {
        let root = std::env::temp_dir().join(format!("rd-install-test-{}", std::process::id()));
        let shell = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join(r"System32\cmd.exe");
        let start = |folder: &str| {
            let path = root.join(folder).join("rd-install-path-test.exe");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::copy(&shell, &path).unwrap();
            let child = Command::new(&path)
                .args(["/D", "/Q"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            (path, child)
        };
        end_running(&root.join("rd-install-absent.exe"));
        let (installed, mut host) = start("installed");
        let (_, mut other) = start("other");
        end_running(&installed);
        let stopped = host.try_wait().unwrap().is_some();
        let spared = other.try_wait().unwrap().is_none();
        host.wait().unwrap();
        other.wait().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(stopped && spared);
    }
}
