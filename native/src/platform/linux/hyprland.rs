//! Hyprland, through `hyprctl`. Its newer configuration is Lua, set with `eval`; the older one takes `keyword`.
//! What is set that way lasts until Hyprland reads its configuration anew, which anything on the desktop may
//! have it do at any time: whoever sets something keeps it set (`keep_settings`).

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Once;
use std::thread;

const SIGNATURE: &str = "HYPRLAND_INSTANCE_SIGNATURE";
/// How Hyprland announces, on its socket for events, that it has read its configuration anew.
const RELOADED: &str = "configreloaded>>";

pub fn running() -> bool {
    std::env::var_os(SIGNATURE).is_some()
}

/// From the first call on, what the host has set in Hyprland is set again (`super::set_again`) whenever
/// Hyprland has read its configuration anew and so forgotten it.
pub fn keep_settings() {
    static LISTENING: Once = Once::new();
    LISTENING.call_once(|| {
        thread::spawn(|| {
            if let Err(error) = listen() {
                warn!("what is set in Hyprland is lost when it reads its configuration anew: {error}");
            }
        });
    });
}

/// Reads Hyprland's events, one a line, for as long as it runs.
fn listen() -> std::io::Result<()> {
    let socket = marker("hypr").join(std::env::var_os(SIGNATURE).unwrap_or_default()).join(".socket2.sock");
    for event in BufReader::new(UnixStream::connect(socket)?).lines() {
        if event?.starts_with(RELOADED) {
            super::set_again();
        }
    }
    Ok(())
}

/// A monitor rule: as the Lua table `lua`, or else in the older form `legacy`.
pub fn monitor(lua: &str, legacy: &str) -> Result<(), String> {
    hyprctl(&["eval", &format!("hl.monitor({lua})")]).or_else(|_| hyprctl(&["keyword", "monitor", legacy]))
}

/// A setting: as the Lua table `lua`, or else as the older form's keyword and value, `legacy`.
pub fn setting(lua: &str, legacy: [&str; 2]) -> Result<(), String> {
    hyprctl(&["eval", &format!("hl.config({lua})")]).or_else(|_| hyprctl(&["keyword", legacy[0], legacy[1]]))
}

/// Runs `hyprctl`, which says "ok" when it did what it was told.
pub fn hyprctl(arguments: &[&str]) -> Result<(), String> {
    let output =
        Command::new("hyprctl").args(arguments).output().map_err(|error| format!("cannot run hyprctl: {error}"))?;
    let reply = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() && reply == "ok" {
        Ok(())
    } else {
        Err(format!("hyprctl {}: {reply}", arguments.join(" ")))
    }
}

/// Where the host notes what it has changed in the desktop, so that one that ended without undoing it, as one
/// that crashed would, undoes it at its next start: in the login session's runtime directory, where Hyprland
/// has its sockets too.
pub fn marker(name: &str) -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from).join(name)
}
