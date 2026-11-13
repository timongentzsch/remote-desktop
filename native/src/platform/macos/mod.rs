//! macOS, being built. Every part the host needs from a platform (see ../mod.rs) is here, and says so where it
//! is not built yet, so that the rest of the host builds and its tests run on a Mac. The parts to come:
//! ScreenCaptureKit for the picture and the sound, CGEvent for keys and the pointer, NSPasteboard for the
//! clipboard, and a LaunchAgent for the install.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

pub use super::unix::{fd, hostname, write_private};
use crate::capture::{Settings, Shared, Sink};
use crate::screen::{Screen, Window};

const NOT_YET: &str = "this part of the host is not built for macOS yet";

fn not_yet<T>() -> Result<T, String> {
    Err(NOT_YET.into())
}

/// Where the password and the certificate are kept: the user's Application Support.
pub fn data_directory() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join("Library/Application Support").join(crate::NAME)
}

pub fn password_path() -> PathBuf {
    data_directory().join("password")
}

pub fn screens() -> Result<Vec<Screen>, String> {
    not_yet()
}

/// Nothing is left changed in the desktop by a host here.
pub fn restore_leftovers() {}

pub mod capture {
    use super::*;

    pub fn run(_settings: Settings, _shared: Arc<Shared>, _sink: Sink) -> Result<(), String> {
        not_yet()
    }
}

pub mod displays {
    /// A sleeping display wakes for the capture by itself.
    pub fn wake_displays(_input: &super::input::Input) {}
}

pub mod pointer {
    /// The capture leaves the pointer out as asked: nothing is hidden.
    pub struct HiddenPointer(());

    pub fn hide() -> Option<HiddenPointer> {
        None
    }
}

pub mod virtual_screen {
    use super::*;

    pub const NAME: &str = crate::NAME;

    /// No screen is made to a client's window here.
    pub struct VirtualScreen(());

    impl VirtualScreen {
        pub fn create(_window: Window, _fps: u32) -> Result<Self, String> {
            not_yet()
        }

        pub fn resize(&self, _window: Window) -> Result<(), String> {
            not_yet()
        }
    }
}

pub mod input {
    use super::*;

    pub struct Input(());

    impl Input {
        pub fn open() -> io::Result<Self> {
            Err(io::Error::new(io::ErrorKind::Unsupported, NOT_YET))
        }

        pub fn key(&self, _code: u16, _down: bool) {}

        pub fn move_to(&self, _x: i32, _y: i32) {}

        pub fn scroll(&self, _horizontal: i32, _vertical: i32) {}
    }
}

pub mod audio {
    use super::*;
    use crate::audio::Sound;

    pub fn run(_shared: Arc<Shared>, _sound: &mut Sound) -> Result<(), String> {
        not_yet()
    }
}

pub mod clipboard {
    use super::*;

    pub type Copied = Box<dyn FnMut(String) + Send>;

    pub struct Inbox(());

    impl Inbox {
        pub fn new() -> io::Result<Arc<Self>> {
            Err(io::Error::new(io::ErrorKind::Unsupported, NOT_YET))
        }

        pub fn put(&self, _text: String) {}
    }

    pub fn run(_inbox: &Inbox, _shared: &Shared, _copied: Copied) -> Result<(), String> {
        not_yet()
    }
}

pub mod greeter {
    /// macOS lets no program show its login window.
    pub fn run(_address: &str) -> ! {
        crate::fail("there is no login screen to show on macOS")
    }
}

pub mod install {
    use crate::fail;

    pub fn install(_tailscale: bool, _no_password: bool) {
        fail(super::NOT_YET)
    }

    pub fn uninstall() {
        fail(super::NOT_YET)
    }

    pub fn renew_certificate(_tailscale: bool) {
        fail(super::NOT_YET)
    }

    pub fn set_password() {
        fail(super::NOT_YET)
    }

    pub fn remove_password() {
        fail(super::NOT_YET)
    }
}

pub mod tcp {
    use std::net::TcpStream;
    use std::os::fd::AsRawFd;
    use std::time::Duration;

    use crate::tls::Control;

    /// Lets the kernel hold no more than `bytes` unsent for the connection, as macOS has it too.
    pub fn limit_unsent(socket: &TcpStream, bytes: i32) {
        unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::IPPROTO_TCP,
                libc::TCP_NOTSENT_LOWAT,
                (&raw const bytes).cast(),
                size_of::<libc::c_int>() as u32,
            )
        };
    }

    pub struct Path {
        pub round_trip: Duration,
        pub retransmitted: u32,
    }

    /// Not read yet (TCP_CONNECTION_INFO has it): the connection keeps the congestion control it has.
    pub fn path(_socket: &TcpStream) -> Option<Path> {
        None
    }

    /// macOS has one congestion control for every connection.
    pub fn set_control(_socket: &TcpStream, _control: Control) {}
}
