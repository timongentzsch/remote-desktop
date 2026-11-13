//! The commands that set the host up and take it away again. The binary does this itself, so nothing else ships.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::{NAME, PORT, certificate, fail, firewall};

const BINARY: &str = "/usr/local/bin/remote-desktop";
/// The user service that runs the host in the desktop session, and the system service at the login screen.
const USER_UNIT: &str = "remote-desktop.service";
const UDEV_RULE_PATH: &str = "/etc/udev/rules.d/70-remote-desktop.rules";
/// Lets whoever is logged in at the seat create input devices. The number must stay below 73.
const UDEV_RULE: &str = r#"KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess", OPTIONS+="static_node=uinput""#;
/// A user service, because capture and input belong to the logged-in desktop session.
fn user_unit() -> String {
    format!(
        "\
[Unit]
Description=Remote desktop host
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart={BINARY}
Restart=on-failure

[Install]
WantedBy=graphical-session.target
"
    )
}

const LOGIN_UNIT: &str = "remote-desktop-login.service";
const SYSTEM_UNITS: &str = "/etc/systemd/system";

/// A system service besides, because the login screen belongs to no user's session (see greeter.rs). It is
/// told where the installing user's password and certificate are.
fn login_unit() -> String {
    format!(
        "\
[Unit]
Description=Remote desktop host at the login screen

[Service]
Environment=XDG_CONFIG_HOME={}
ExecStart={BINARY} login-screen
Restart=always
RestartSec=1

[Install]
WantedBy=multi-user.target
",
        config_directory().display()
    )
}

/// The program's desktop entry, hidden from menus. Plasma before 6.8 only casts the screen for a program whose
/// entry asks for that (see capture/kwin.rs), and a copy among the desktop's autostart entries starts the
/// host at login where systemd does not.
fn desktop_entry() -> String {
    format!(
        "\
[Desktop Entry]
Type=Application
Name=Remote Desktop
Exec={BINARY}
NoDisplay=true
X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1
"
    )
}

/// One of the user's base directories: where `variable` points, or `fallback` in the home directory.
fn user_directory(variable: &str, fallback: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(fallback)))
        .unwrap_or_else(|| fail(&format!("neither {variable} nor HOME is set")))
}

fn desktop_entry_path() -> PathBuf {
    user_directory("XDG_DATA_HOME", ".local/share").join("applications").join(format!("{NAME}.desktop"))
}

fn login_unit_path() -> String {
    format!("{SYSTEM_UNITS}/{LOGIN_UNIT}")
}

pub fn config_directory() -> PathBuf {
    user_directory("XDG_CONFIG_HOME", ".config")
}

/// Where the password and the certificate are kept.
pub fn data_directory() -> PathBuf {
    config_directory().join(NAME)
}

pub fn password_path() -> PathBuf {
    data_directory().join("password")
}

/// Whether systemd runs the user's services. Where it does not, the desktop's autostart starts the host.
fn has_systemd() -> bool {
    Command::new("systemctl").args(["--user", "show-environment"]).output().is_ok_and(|output| output.status.success())
}

/// What the user is told to run for a change of password or certificate to take effect.
fn restart_command() -> String {
    if has_systemd() { format!("systemctl --user restart {USER_UNIT}") } else { format!("{NAME} install") }
}

fn unit_path() -> PathBuf {
    config_directory().join("systemd/user").join(USER_UNIT)
}

fn autostart_path() -> PathBuf {
    config_directory().join("autostart").join(format!("{NAME}.desktop"))
}

fn write(path: PathBuf, contents: &str) {
    fs::create_dir_all(path.parent().unwrap())
        .and_then(|()| fs::write(&path, contents))
        .unwrap_or_else(|error| fail(&format!("cannot write {}: {error}", path.display())));
}

/// Stops a host that was started without systemd: the process whose whole command line is the binary's path.
fn stop_host() {
    let _ = Command::new("pkill").args(["-x", "-f", BINARY]).status();
}

/// Copies this binary into place, grants access to uinput (both through sudo) and starts the host: as a
/// service of the user's systemd, or without one now and through the desktop's autostart at each login.
pub fn install(tailscale: bool, no_password: bool) {
    if no_password {
        clear_password();
    } else if !password_path().exists() {
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
    if tailscale {
        use_tailscale();
    } else if !certificate::path().exists() {
        certificate::create().unwrap_or_else(|error| fail(&error));
    }
    // A certificate from Tailscale stays when installing again without --tailscale, and so does what it implies.
    let tailnet_only = certificate::tailscale_name().is_some();
    firewall::open(tailnet_only);

    write(desktop_entry_path(), &desktop_entry());
    if has_systemd() {
        write(unit_path(), &user_unit());
        run("systemctl", &["--user", "daemon-reload"]);
        run("systemctl", &["--user", "enable", USER_UNIT]);
        run("systemctl", &["--user", "restart", USER_UNIT]);
        let draft = data_directory().join(LOGIN_UNIT); // where no one else writes
        write(draft.clone(), &login_unit());
        run("sudo", &["install", "-m644", &draft.to_string_lossy(), &login_unit_path()]);
        let _ = fs::remove_file(draft);
        run("sudo", &["systemctl", "daemon-reload"]);
        run("sudo", &["systemctl", "--quiet", "enable", LOGIN_UNIT]);
        run("sudo", &["systemctl", "restart", LOGIN_UNIT]);
        println!("Installed {BINARY} and started the {USER_UNIT} user service,");
        println!("and the {LOGIN_UNIT} system service for the login screen.");
    } else {
        write(autostart_path(), &desktop_entry());
        stop_host();
        // In a session of its own, so that it outlives this terminal; `setsid` leaves once it has started it.
        if !run_quietly("setsid", &["--fork", BINARY]) {
            fail(&format!("cannot start {BINARY}"));
        }
        println!("Installed {BINARY} and started it. From now on it starts with the desktop session, at login.");
    }
    if fs::read(password_path()).is_ok_and(|password| password.trim_ascii().is_empty()) {
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
    // Whichever way it was started; what is not there to stop or remove is fine.
    if has_systemd() {
        run_quietly("systemctl", &["--user", "--quiet", "disable", "--now", USER_UNIT]);
        run_quietly("sudo", &["systemctl", "--quiet", "disable", "--now", LOGIN_UNIT]);
    }
    stop_host();
    for path in [unit_path(), autostart_path(), desktop_entry_path()] {
        let _ = fs::remove_file(path);
    }
    if has_systemd() {
        run("systemctl", &["--user", "daemon-reload"]);
    }
    firewall::close();
    run("sudo", &["rm", "-f", BINARY, UDEV_RULE_PATH, &login_unit_path()]);
    run("sudo", &["udevadm", "control", "--reload"]);
    let kept = data_directory();
    println!(
        "Removed the services, {BINARY} and the input rule. The password and certificate remain in {}.",
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
    // Who the host answers follows where the certificate comes from, and the firewall with it.
    if std::path::Path::new(BINARY).exists() {
        firewall::open(tailscale);
    }
    println!("The certificate changed. Restart the host to use it: {}", restart_command());
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
        println!("\nOpen https://{name}:{} in Chrome from any device on your tailnet.", PORT);
        return;
    }
    let path = certificate::path();
    println!(
        "\nOpen https://{}:{} in Chrome. It also answers to: {}",
        certificate::names()[0],
        PORT,
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
    clear_password();
    println!("No password is needed any more: anyone who can reach port {} on this machine can see and control", PORT);
    println!("this desktop. Restart the host to apply: {}", restart_command());
}

/// An empty password: no password is asked for.
fn clear_password() {
    write_private(&password_path(), b"").unwrap_or_else(|error| fail(&error));
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
