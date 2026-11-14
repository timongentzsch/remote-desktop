//! Linux: Wayland compositors (GNOME, KDE, Hyprland and those built on wlroots) and X11, PipeWire and
//! PulseAudio, uinput, systemd and logind, ufw and firewalld.

pub mod audio;
pub mod capture;
pub mod clipboard;
mod compositor;
pub mod displays;
mod firewall;
mod gnome;
pub mod greeter;
mod hyprland;
pub mod input;
pub mod install;
pub mod pointer;
pub mod tcp;
mod typing;
pub mod virtual_screen;
mod xserver;

pub use super::unix::{fd, hostname, write_private};
pub use install::{data_directory, password_path};

use crate::screen::Screen;

/// Undoes what a host that ended without undoing it, as one that crashed would, left changed in the desktop.
pub fn restore_leftovers() {
    virtual_screen::restore_leftover();
    pointer::restore_leftover();
}

/// The desktop's screens, as its compositor or X server tells them.
pub fn screens() -> Result<Vec<Screen>, String> {
    if compositor::available() { compositor::screens() } else { xserver::screens() }
}
