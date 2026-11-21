//! Installing on Windows: not built yet.

use crate::fail;

pub const BINARY: &str = r"%LOCALAPPDATA%\Programs\remote-desktop\remote-desktop.exe";

pub fn restart_command() -> String {
    "remote-desktop install".into()
}

pub fn start(_tailnet_only: bool) {
    fail(super::NOT_YET)
}

pub fn stop() {
    fail(super::NOT_YET)
}

pub fn follow_certificate(_tailnet_only: bool) {}

pub fn allow_tailscale() {}
