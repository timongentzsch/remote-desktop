//! `remote-desktop install` and `remote-desktop password`: the binary sets itself up, so nothing else ships.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::Command;

use crate::{certificate, fail};

const BINARY: &str = "/usr/local/bin/remote-desktop";
const UDEV_RULE_PATH: &str = "/etc/udev/rules.d/70-remote-desktop.rules";
/// Lets whoever is logged in at the seat create input devices. The number must stay below 73.
const UDEV_RULE: &str = r#"KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess", OPTIONS+="static_node=uinput""#;
/// A user service, because capture and input belong to the logged-in desktop session.
const UNIT: &str = "\
[Unit]
Description=Remote desktop host
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart=/usr/local/bin/remote-desktop
Restart=on-failure

[Install]
WantedBy=graphical-session.target
";

/// Plasma before 6.8 only casts the screen for a program whose desktop file asks for that (see kwin.rs). The
/// entry is hidden from menus.
const DESKTOP_ENTRY: &str = "\
[Desktop Entry]
Type=Application
Name=Remote Desktop
Exec=/usr/local/bin/remote-desktop
NoDisplay=true
X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1
";

/// One of the user's base directories: where `variable` points, or `fallback` in the home directory.
fn user_directory(variable: &str, fallback: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(fallback)))
        .unwrap_or_else(|| fail(&format!("neither {variable} nor HOME is set")))
}

fn desktop_entry_path() -> PathBuf {
    user_directory("XDG_DATA_HOME", ".local/share").join("applications/remote-desktop.desktop")
}

pub fn config_directory() -> PathBuf {
    user_directory("XDG_CONFIG_HOME", ".config")
}

pub fn password_path() -> PathBuf {
    config_directory().join("remote-desktop/password")
}

/// Copies this binary into place, grants access to uinput (both through sudo) and starts the user service.
pub fn install(tailscale: bool) {
    if !password_path().exists() {
        set_password();
    }
    let this = std::env::current_exe().unwrap_or_else(|error| fail(&format!("cannot locate this program: {error}")));
    run("sudo", &["install", "-Dm755", &this.to_string_lossy(), BINARY]);
    let rule = format!("printf '%s\\n' '{UDEV_RULE}' > {UDEV_RULE_PATH}");
    run(
        "sudo",
        &[
            "sh",
            "-c",
            &format!("{rule} && modprobe uinput && udevadm control --reload && udevadm trigger --name-match=uinput"),
        ],
    );
    firewall(true);
    if tailscale {
        use_tailscale();
    } else if !certificate::path().exists() {
        certificate::create().unwrap_or_else(|error| fail(&error));
    }

    let unit = config_directory().join("systemd/user/remote-desktop.service");
    for (path, contents) in [(unit, UNIT), (desktop_entry_path(), DESKTOP_ENTRY)] {
        fs::create_dir_all(path.parent().unwrap())
            .and_then(|()| fs::write(&path, contents))
            .unwrap_or_else(|error| fail(&format!("cannot write {}: {error}", path.display())));
    }
    run("systemctl", &["--user", "daemon-reload"]);
    run("systemctl", &["--user", "enable", "remote-desktop.service"]);
    run("systemctl", &["--user", "restart", "remote-desktop.service"]);
    println!("Installed {BINARY} and started the remote-desktop user service.");
    show_certificate();
}

/// Opens or closes the service's port where a firewall this knows how to configure is active.
fn firewall(open: bool) {
    let port = format!("{}/tcp", crate::PORT);
    let active = |program: &str, arguments: &[&str], marker: &str| {
        Command::new("sudo")
            .arg(program)
            .args(arguments)
            .output()
            .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains(marker))
    };
    let done = if open { "Opened" } else { "Closed" };
    if active("ufw", &["status"], "Status: active") {
        let rule: &[&str] = if open { &["ufw", "allow"] } else { &["ufw", "delete", "allow"] };
        run("sudo", &[rule, &[&port]].concat());
        println!("{done} port {port} in the firewall (ufw).");
    } else if active("firewall-cmd", &["--state"], "running") {
        let change = format!("--{}-port={port}", if open { "add" } else { "remove" });
        run("sudo", &["firewall-cmd", "--quiet", "--permanent", &change]);
        run("sudo", &["firewall-cmd", "--quiet", "--reload"]);
        println!("{done} port {port} in the firewall (firewalld).");
    }
}

/// `remote-desktop uninstall`: undoes `install`. The password and certificate stay, so installing again keeps
/// the address clients already trust.
pub fn uninstall() {
    // The service may already be gone, which is fine.
    let _ =
        Command::new("systemctl").args(["--user", "--quiet", "disable", "--now", "remote-desktop.service"]).status();
    let _ = fs::remove_file(config_directory().join("systemd/user/remote-desktop.service"));
    let _ = fs::remove_file(desktop_entry_path());
    run("systemctl", &["--user", "daemon-reload"]);
    firewall(false);
    run("sudo", &["rm", "-f", BINARY, UDEV_RULE_PATH]);
    run("sudo", &["udevadm", "control", "--reload"]);
    let kept = config_directory().join("remote-desktop");
    println!(
        "Removed the service, {BINARY} and the input rule. The password and certificate remain in {}.",
        kept.display()
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
    println!("The certificate changed. Restart the service to use it: systemctl --user restart remote-desktop");
    show_certificate();
}

/// The service runs as this user and renews the certificate itself, which Tailscale only lets its operator do.
fn use_tailscale() {
    let user = std::env::var("USER").unwrap_or_else(|_| fail("USER is not set"));
    run("sudo", &["tailscale", "set", &format!("--operator={user}")]);
    certificate::use_tailscale().unwrap_or_else(|error| fail(&error));
}

/// Tells the user where to connect and, for a self-signed certificate, how to make their browser trust it.
fn show_certificate() {
    if let Some(name) = certificate::tailscale_name() {
        println!("\nOpen https://{name}:{} in Chrome from any device on your tailnet.", crate::PORT);
        return;
    }
    let path = certificate::path();
    println!(
        "\nOpen https://{}:{} in Chrome. It also answers to: {}",
        certificate::names()[0],
        crate::PORT,
        certificate::names()[1..].join(", ")
    );
    println!("The certificate is self-signed, so each client has to trust it once. On a Mac:");
    println!("    scp <this machine>:{} remote-desktop.pem", path.display());
    println!(
        "    security add-trusted-cert -r trustRoot -p ssl -k ~/Library/Keychains/login.keychain-db remote-desktop.pem"
    );
}

/// Reads a password from the terminal (not echoed) or from standard input and stores it for the service.
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
    write_private(&password_path(), b"").unwrap_or_else(|error| fail(&error));
    println!(
        "No password is needed any more: anyone who can reach port {} on this machine can see and control",
        crate::PORT
    );
    println!("this desktop. Restart the service to apply: systemctl --user restart remote-desktop");
}

/// Writes a file only its owner can read, creating the directory it lives in.
pub fn write_private(path: &std::path::Path, contents: &[u8]) -> Result<(), String> {
    fs::create_dir_all(path.parent().unwrap())
        .and_then(|()| fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path))
        .and_then(|mut file| file.write_all(contents))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
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

fn run(program: &str, arguments: &[&str]) {
    match Command::new(program).args(arguments).status() {
        Ok(status) if status.success() => {}
        Ok(status) => fail(&format!("`{program} {}` failed ({status})", arguments.join(" "))),
        Err(error) => fail(&format!("cannot run {program}: {error}")),
    }
}
