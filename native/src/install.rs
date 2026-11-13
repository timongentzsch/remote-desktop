//! The commands that set the host up and take it away again. The binary does this itself, so nothing else ships.
//! What differs between systems, putting the binary in place and running it as a service, is the platform's
//! (see platform/*/install.rs); the password and the certificate are the same everywhere.

use std::io::{self, Write};
use std::process::{Command, Stdio};

use crate::platform::install::{self as service, BINARY};
use crate::platform::{password_path, write_private};
use crate::{PORT, certificate, fail};

/// `remote-desktop install`: asks for a password unless there is one, makes the certificate unless there is one,
/// and leaves the host running as a service.
pub fn install(tailscale: bool, no_password: bool) {
    if no_password {
        clear_password();
    } else if !password_path().exists() {
        set_password();
    }
    if tailscale {
        use_tailscale();
    } else if !certificate::path().exists() {
        certificate::create().unwrap_or_else(|error| fail(&error));
    }
    // A certificate from Tailscale stays when installing again without --tailscale, and so does what it implies.
    let tailnet_only = certificate::tailscale_name().is_some();
    service::start(tailnet_only);
    if std::fs::read(password_path()).is_ok_and(|password| password.trim_ascii().is_empty()) {
        let who = if tailnet_only {
            "on your tailnet who may reach this machine".into()
        } else {
            format!("who can reach port {PORT}")
        };
        println!("No password is set: anyone {who} can see and control this desktop.");
    }
    show_certificate();
}

/// `remote-desktop uninstall`: undoes `install`. The password and certificate stay, so installing again keeps
/// the address clients already trust.
pub fn uninstall() {
    service::stop();
    println!(
        "Removed the service and {BINARY}. The password and certificate remain in {}.",
        password_path().parent().unwrap().display()
    );
}

/// `remote-desktop certificate [--tailscale]`: makes a fresh self-signed certificate, for when the machine's
/// name or address changed, or switches to one from Tailscale.
pub fn renew_certificate(tailscale: bool) {
    if tailscale {
        use_tailscale();
    } else {
        certificate::create().unwrap_or_else(|error| fail(&error));
    }
    service::follow_certificate(tailscale);
    println!("The certificate changed. Restart the host to use it: {}", service::restart_command());
    show_certificate();
}

fn use_tailscale() {
    service::allow_tailscale();
    certificate::use_tailscale().unwrap_or_else(|error| fail(&error));
}

/// Tells the user where to connect and, for a self-signed certificate, how to make their browser trust it.
fn show_certificate() {
    if let Some(name) = certificate::tailscale_name() {
        println!("\nOpen https://{name}:{PORT} in Chrome from any device on your tailnet.");
        return;
    }
    let names = certificate::names();
    println!("\nOpen https://{}:{PORT} in Chrome. It also answers to: {}", names[0], names[1..].join(", "));
    println!("The certificate is self-signed, so each client has to trust it once. On a Mac:");
    println!("    scp <this machine>:{} remote-desktop.pem", certificate::path().display());
    println!(
        "    security add-trusted-cert -r trustRoot -p ssl -k ~/Library/Keychains/login.keychain-db remote-desktop.pem"
    );
}

/// `remote-desktop password`: reads a password from the terminal (not echoed) or from standard input and stores
/// it for the service.
pub fn set_password() {
    eprint!("Password for remote access: ");
    let _ = io::stderr().flush();
    let mut line = String::new();
    let read = without_echo(|| io::stdin().read_line(&mut line));
    eprintln!();
    let password = line.trim_end_matches(['\r', '\n']);
    if read.is_err() || password.is_empty() {
        fail("the password cannot be empty");
    }
    write_private(&password_path(), password.as_bytes()).unwrap_or_else(|error| fail(&error));
}

/// `remote-desktop password --none`: lets anyone who can reach this machine's port control it.
pub fn remove_password() {
    clear_password();
    println!("No password is needed any more: anyone who can reach port {PORT} on this machine can see and control");
    println!("this desktop. Restart the host to apply: {}", service::restart_command());
}

/// An empty password: no password is asked for.
fn clear_password() {
    write_private(&password_path(), b"").unwrap_or_else(|error| fail(&error));
}

/// Runs `action` with terminal echo off, if standard input is a terminal.
fn without_echo<T>(action: impl FnOnce() -> T) -> T {
    let mut terminal = unsafe { std::mem::zeroed::<libc::termios>() };
    let is_terminal = unsafe { libc::tcgetattr(0, &mut terminal) } == 0;
    if is_terminal {
        let mut silent = terminal;
        silent.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &silent) };
    }
    let result = action();
    if is_terminal {
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &terminal) };
    }
    result
}

/// Runs a command that has to succeed, or else ends with what went wrong.
pub fn run(program: &str, arguments: &[&str]) {
    match Command::new(program).args(arguments).status() {
        Ok(status) if status.success() => {}
        Ok(status) => fail(&format!("`{program} {}` failed ({status})", arguments.join(" "))),
        Err(error) => fail(&format!("cannot run {program}: {error}")),
    }
}

/// Runs a command whose failure is fine, such as removing what may not be there, without its output.
pub fn run_quietly(program: &str, arguments: &[&str]) -> bool {
    let silent = Stdio::null;
    Command::new(program)
        .args(arguments)
        .stdin(silent())
        .stdout(silent())
        .stderr(silent())
        .status()
        .is_ok_and(|status| status.success())
}

/// Writes a file, making its directory, or else ends with what went wrong.
pub fn write(path: &std::path::Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap())
        .and_then(|()| std::fs::write(path, contents))
        .unwrap_or_else(|error| fail(&format!("cannot write {}: {error}", path.display())));
}
