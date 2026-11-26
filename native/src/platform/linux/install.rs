//! Installing on Linux: the binary in /usr/local/bin, access to uinput, the firewall, and services of systemd or
//! the desktop's autostart (see ../../install.rs for the rest).

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use crate::install::{run, run_quietly, write};
use crate::platform::linux::firewall;
use crate::{NAME, fail, port, update};

pub use firewall::check_port;

pub const BINARY: &str = "/usr/local/bin/remote-desktop";
/// The user service that runs the host in the desktop session, and the system service at the login screen.
const USER_UNIT: &str = "remote-desktop.service";
const UDEV_RULE_PATH: &str = "/etc/udev/rules.d/70-remote-desktop.rules";
/// What lets the binary listen on a port that Linux keeps for root, as `setcap` is told it.
const PORT_CAPABILITY: &str = "cap_net_bind_service=ep";
/// Lets whoever is logged in at the seat create input devices. The number must stay below 73.
const UDEV_RULE: &str = r#"KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess", OPTIONS+="static_node=uinput""#;
/// A user service, because capture and input belong to the logged-in desktop session. It starts with the
/// user's other services and finds that session itself (see greeter.rs): not every desktop tells systemd when
/// it is up.
fn user_unit() -> String {
    format!(
        "\
[Unit]
Description=Remote desktop host

[Service]
ExecStart={BINARY}
Restart=on-failure

[Install]
WantedBy=default.target
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

/// Two more system units where newer versions install themselves. The host cannot replace its own binary,
/// which is root's: it writes the version it wants where the first unit watches, and that starts the second,
/// which is this program as root (see update.rs). It is told where the user's settings are, for the port.
const UPDATE_WATCH: &str = "remote-desktop-update.path";
const UPDATE_UNIT: &str = "remote-desktop-update.service";

fn update_watch() -> String {
    format!(
        "\
[Unit]
Description=Remote desktop host asks for a newer version

[Path]
PathModified={}

[Install]
WantedBy=paths.target
",
        update::wanted_path().display()
    )
}

fn update_unit() -> String {
    format!(
        "\
[Unit]
Description=Remote desktop host's newer version

[Service]
Type=oneshot
Environment=XDG_CONFIG_HOME={}
ExecStart={BINARY} update
",
        config_directory().display()
    )
}

/// The program's desktop entry, hidden from menus. Plasma before 6.8 only casts the screen for a program whose
/// entry asks for that (see capture/kwin.rs), and a copy among the desktop's autostart entries starts the
/// host at login where systemd runs no services for the user.
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

fn system_unit_path(unit: &str) -> String {
    format!("{SYSTEM_UNITS}/{unit}")
}

/// Puts a unit among the system's, through sudo.
fn install_system_unit(unit: &str, contents: &str) {
    let draft = data_directory().join(unit); // where no one else writes
    write(&draft, contents);
    run("sudo", &["install", "-m644", &draft.to_string_lossy(), &system_unit_path(unit)]);
    let _ = fs::remove_file(draft);
}

pub fn config_directory() -> PathBuf {
    user_directory("XDG_CONFIG_HOME", ".config")
}

/// Where the password and the certificate are kept.
pub fn data_directory() -> PathBuf {
    config_directory().join(NAME)
}

/// Whether systemd runs the user's services. Where it does not, the desktop's autostart starts the host.
fn has_systemd() -> bool {
    Command::new("systemctl").args(["--user", "show-environment"]).output().is_ok_and(|output| output.status.success())
}

pub fn binary() -> PathBuf {
    BINARY.into()
}

/// Whether newer versions can install themselves, or else why not.
pub fn updates_itself() -> Result<(), &'static str> {
    if has_systemd() { Ok(()) } else { Err("that takes systemd. `remote-desktop update` installs one.") }
}

/// Whether the host's port is one that Linux keeps for root and for programs given the capability.
fn port_is_reserved() -> bool {
    let unprivileged = fs::read_to_string("/proc/sys/net/ipv4/ip_unprivileged_port_start")
        .ok()
        .and_then(|port| port.trim().parse::<u16>().ok())
        .unwrap_or(1024);
    port() < unprivileged
}

/// Nothing has to stop before the binary changes: a running host keeps the file it was started from.
pub fn stop_for_update() {}

/// As root, once the binary has changed: the new file gets the capability the old one had, and the hosts
/// start again from it, the one at the login screen and that of the user whose settings these are.
pub fn start_updated() {
    if port_is_reserved() {
        run_quietly("setcap", &[PORT_CAPABILITY, BINARY]);
    }
    restart_service(|| Command::new("systemctl"), LOGIN_UNIT);
    if let Ok(owner) = fs::metadata(config_directory()) {
        let users = || {
            let mut systemctl = Command::new("systemctl");
            systemctl.arg("--user").uid(owner.uid()).gid(owner.gid());
            systemctl.env("XDG_RUNTIME_DIR", format!("/run/user/{}", owner.uid()));
            systemctl
        };
        restart_service(users, USER_UNIT);
    }
}

/// Restarts a service that runs, and one that systemd restarts over and over or has given up on, as it does
/// with a binary that does not start. One that was not running stays so.
fn restart_service(systemctl: impl Fn() -> Command, unit: &str) {
    let state = systemctl().args(["is-active", unit]).output().map(|output| output.stdout).unwrap_or_default();
    if matches!(state.trim_ascii(), b"active" | b"activating" | b"failed") {
        let _ = systemctl().args(["reset-failed", unit]).status();
        let _ = systemctl().args(["restart", unit]).status();
    }
}

/// What the user is told to run for a change of password or certificate to take effect.
pub fn restart_command() -> String {
    if has_systemd() { format!("systemctl --user restart {USER_UNIT}") } else { format!("{NAME} install") }
}

fn unit_path() -> PathBuf {
    config_directory().join("systemd/user").join(USER_UNIT)
}

fn autostart_path() -> PathBuf {
    config_directory().join("autostart").join(format!("{NAME}.desktop"))
}

/// Stops a host that was started without systemd: the process whose whole command line is the binary's path.
fn stop_host() {
    let _ = Command::new("pkill").args(["-x", "-f", BINARY]).status();
}

/// Copies this binary into place, grants access to uinput (both through sudo) and starts the host: as a
/// service of the user's systemd, or without one now and through the desktop's autostart at each login.
pub fn start(tailnet_only: bool) {
    let this = std::env::current_exe().unwrap_or_else(|error| fail(&format!("cannot locate this program: {error}")));
    if this != std::path::Path::new(BINARY) {
        run("sudo", &["install", "-Dm755", &this.to_string_lossy(), BINARY]);
    }
    if port_is_reserved() {
        run("sudo", &["setcap", PORT_CAPABILITY, BINARY]);
    }
    let rule = format!("printf '%s\\n' '{UDEV_RULE}' > {UDEV_RULE_PATH}");
    run(
        "sudo",
        &[
            "sh",
            "-c",
            &format!("{rule} && modprobe uinput && udevadm control --reload && udevadm trigger --name-match=uinput"),
        ],
    );
    firewall::open(tailnet_only);

    write(&desktop_entry_path(), &desktop_entry());
    if has_systemd() {
        // What earlier versions had start the service goes: the session's target, and from 0.11.4 the
        // desktop's autostart beside it.
        run_quietly("systemctl", &["--user", "--quiet", "disable", USER_UNIT]);
        let _ = fs::remove_file(autostart_path());
        write(&unit_path(), &user_unit());
        run("systemctl", &["--user", "daemon-reload"]);
        run("systemctl", &["--user", "enable", USER_UNIT]);
        run("systemctl", &["--user", "restart", USER_UNIT]);
        install_system_unit(LOGIN_UNIT, &login_unit());
        let updates = update::wanted_path().exists();
        if updates {
            install_system_unit(UPDATE_WATCH, &update_watch());
            install_system_unit(UPDATE_UNIT, &update_unit());
        }
        run("sudo", &["systemctl", "daemon-reload"]);
        run("sudo", &["systemctl", "--quiet", "enable", LOGIN_UNIT]);
        run("sudo", &["systemctl", "restart", LOGIN_UNIT]);
        if updates {
            run("sudo", &["systemctl", "--quiet", "enable", "--now", UPDATE_WATCH]);
        }
        println!("Installed {BINARY} and started the {USER_UNIT} user service,");
        println!("and the {LOGIN_UNIT} system service for the login screen.");
    } else {
        write(&autostart_path(), &desktop_entry());
        stop_host();
        // In a session of its own, so that it outlives this terminal; `setsid` leaves once it has started it.
        if !run_quietly("setsid", &["--fork", BINARY]) {
            fail(&format!("cannot start {BINARY}"));
        }
        println!("Installed {BINARY} and started it. From now on it starts with the desktop session, at login.");
    }
}

/// Undoes `start`.
pub fn stop() {
    // Whichever way it was started; what is not there to stop or remove is fine.
    if has_systemd() {
        run_quietly("systemctl", &["--user", "--quiet", "disable", "--now", USER_UNIT]);
        run_quietly("sudo", &["systemctl", "--quiet", "disable", "--now", LOGIN_UNIT, UPDATE_WATCH]);
    }
    stop_host();
    for path in [unit_path(), autostart_path(), desktop_entry_path()] {
        let _ = fs::remove_file(path);
    }
    if has_systemd() {
        run("systemctl", &["--user", "daemon-reload"]);
    }
    firewall::close();
    let units = [LOGIN_UNIT, UPDATE_WATCH, UPDATE_UNIT].map(system_unit_path);
    run("sudo", &["rm", "-f", BINARY, UDEV_RULE_PATH, &units[0], &units[1], &units[2]]);
    run("sudo", &["udevadm", "control", "--reload"]);
}

/// Who the host answers follows where the certificate comes from, and the firewall with it.
pub fn follow_certificate(tailnet_only: bool) {
    if std::path::Path::new(BINARY).exists() {
        firewall::follow_certificate(tailnet_only);
    }
}

/// The service runs as this user and renews the certificate itself, which Tailscale only lets its operator do.
pub fn allow_tailscale() {
    let user = std::env::var("USER").unwrap_or_else(|_| fail("USER is not set"));
    run("sudo", &["tailscale", "set", &format!("--operator={user}")]);
}
