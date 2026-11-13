//! Linux: Wayland compositors (GNOME, KDE, Hyprland and those built on wlroots) and X11, PipeWire and
//! PulseAudio, uinput, systemd and logind, ufw and firewalld.

pub mod audio;
pub mod capture;
pub mod clipboard;
mod compositor;
pub mod displays;
pub mod fd;
mod firewall;
mod gnome;
pub mod greeter;
pub mod input;
pub mod install;
pub mod tcp;
pub mod virtual_screen;
mod xserver;

pub use install::{data_directory, password_path, write_private};

use crate::screen::Screen;

/// The desktop's screens, as its compositor or X server tells them.
pub fn screens() -> Result<Vec<Screen>, String> {
    if compositor::available() { compositor::screens() } else { xserver::screens() }
}

/// The machine's name, or nothing where it has none.
pub fn hostname() -> String {
    let mut buffer = [0u8; 256];
    match unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len() - 1) } {
        0 => String::from_utf8_lossy(&buffer[..buffer.iter().position(|&byte| byte == 0).unwrap_or(0)]).into_owned(),
        _ => String::new(),
    }
}
