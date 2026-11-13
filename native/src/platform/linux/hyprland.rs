//! Hyprland, through `hyprctl`. Its newer configuration is Lua, set with `eval`; the older one takes `keyword`.

use std::process::Command;

pub fn running() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
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
/// that crashed would, undoes it at its next start: in the login session's runtime directory.
pub fn marker(name: &str) -> std::path::PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, std::path::PathBuf::from).join(name)
}
