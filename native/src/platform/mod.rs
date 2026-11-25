//! Everything that differs from one operating system to another, behind the one surface the rest of the host
//! uses. Each system is a module here that provides these same names, chosen when the host is built: Linux,
//! macOS and Windows. The rest of the host (the server and sessions, the bitrate, QUIC, the stream and its
//! encoders, and the client) is the same everywhere. What Linux and macOS do alike is in unix/.

pub mod tcp;
#[cfg(unix)]
mod unix;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as os;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as os;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as os;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
compile_error!("there is no platform module for this operating system yet: see platform/mod.rs");

// The screen: its picture for a session (`capture(settings, shared, sink)`, see capture.rs), the screens there
// are (`screens()`), waking those that sleep (`wake_displays(&input)`), a screen made to the client's window
// where the desktop supports it (`virtual_screen::VirtualScreen`, for a screen::Window), the desktop's pointer
// hidden while a client draws its own where the picture would show it too (`hide_pointer()`), and undoing
// what a host that crashed left changed (`restore_leftovers()`).
#[cfg(not(target_os = "linux"))]
pub use display::{capture, hide_pointer, restore_leftovers, wake_displays};
#[cfg(target_os = "linux")]
pub use linux::{capture::run as capture, displays::wake_displays, pointer::hide as hide_pointer, restore_leftovers};
pub use os::screens;
pub use os::virtual_screen;

#[cfg(not(target_os = "linux"))]
mod display {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use crate::capture::{Settings, Shared, Sink};
    use crate::stream::Stream;

    /// Reopen capture when the display changes. A mode switch, virtual display or secure desktop may
    /// temporarily refuse capture while the session is still wanted.
    pub fn capture(settings: Settings, shared: Arc<Shared>, sink: Sink) -> Result<(), String> {
        const REFUSING: Duration = Duration::from_secs(8);
        let mut stream = Stream::new(settings, shared, sink);
        let mut refused = None;
        while stream.wanted() {
            match super::os::capture::run(&mut stream) {
                Ok(()) => refused = None,
                Err(error) if refused.get_or_insert_with(Instant::now).elapsed() > REFUSING => return Err(error),
                Err(_) => std::thread::sleep(stream.frame_interval()),
            }
        }
        stream.ended.take().unwrap_or(Ok(()))
    }

    // Linux needs compositor-specific hooks to wake displays and hide the pointer.
    pub fn wake_displays(_input: &super::Input) {}
    pub fn hide_pointer() -> Option<()> {
        None
    }
    /// A Windows display keeps the mode and scale a client gave it when the host ends without undoing them.
    pub fn restore_leftovers() {
        #[cfg(windows)]
        super::os::virtual_screen::restore_leftover();
    }
}

// What the client does: keys, buttons and the pointer, in the client's codes, which are Linux's (linux/input.rs).
pub use os::input::Input;

// What the session sends along: the desktop's sound, 48 kHz stereo samples that `record_sound(shared, sound)`
// hands to the encoder (audio.rs), and clipboard text both ways (`clipboard::{Inbox, run}`).
pub use os::audio::run as record_sound;
pub use os::clipboard;

// Setting the host up: the `install`, `uninstall`, `password` and `certificate` commands, and the host at the
// login screen (`login_screen(address)`).
pub use os::greeter::run as login_screen;
pub use os::install;

// Where the password and certificate are kept, the machine's name, typing the password unseen, and a host that
// runs without a terminal saying what it says somewhere it is kept (`serve_quietly()`).
pub use os::{data_directory, hostname, serve_quietly, without_echo, write_private};

pub fn password_path() -> std::path::PathBuf {
    data_directory().join("password")
}

// Waiting on a socket (`socket_readable`), and on a socket or a wake from another thread (`Waker`); the
// kernel's side of a TCP connection (`tcp`).
pub use os::fd::{Waker, socket_readable};
