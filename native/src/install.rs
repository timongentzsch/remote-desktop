//! `remote-desktop install` and `remote-desktop password`: the binary sets itself up, so nothing else ships.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::Command;

use crate::fail;

const BINARY: &str = "/usr/local/bin/remote-desktop";
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

fn config_directory() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".config")))
        .unwrap_or_else(|| fail("neither XDG_CONFIG_HOME nor HOME is set"))
}

pub fn password_path() -> PathBuf {
    config_directory().join("remote-desktop/password")
}

/// Copies this binary into place, grants access to uinput (both through sudo) and starts the user service.
pub fn install() {
    if !password_path().exists() {
        set_password();
    }
    let this = std::env::current_exe().unwrap_or_else(|error| fail(&format!("cannot locate this program: {error}")));
    run("sudo", &["install", "-Dm755", &this.to_string_lossy(), BINARY]);
    let rule = format!("printf '%s\\n' '{UDEV_RULE}' > /etc/udev/rules.d/70-remote-desktop.rules");
    run("sudo", &["sh", "-c", &format!("{rule} && modprobe uinput && udevadm control --reload && udevadm trigger --name-match=uinput")]);

    let unit = config_directory().join("systemd/user/remote-desktop.service");
    fs::create_dir_all(unit.parent().unwrap())
        .and_then(|()| fs::write(&unit, UNIT))
        .unwrap_or_else(|error| fail(&format!("cannot write {}: {error}", unit.display())));
    run("systemctl", &["--user", "daemon-reload"]);
    run("systemctl", &["--user", "enable", "remote-desktop.service"]);
    run("systemctl", &["--user", "restart", "remote-desktop.service"]);
    println!("Installed {BINARY} and started the remote-desktop user service on 127.0.0.1:8765.");
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
    let path = password_path();
    fs::create_dir_all(path.parent().unwrap())
        .and_then(|()| fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path))
        .and_then(|mut file| file.write_all(password.as_bytes()))
        .unwrap_or_else(|error| fail(&format!("cannot write {}: {error}", path.display())));
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
