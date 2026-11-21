//! Windows: so far only what the host needs to build and run its tests. The picture, input, sound, the
//! clipboard and installing are not built yet, and say so.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use windows::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
};

use crate::capture::Shared;
use crate::screen::Screen;

pub mod fd;
pub mod install;

const NOT_YET: &str = "this part of the host is not built for Windows yet";

fn not_yet<T>() -> Result<T, String> {
    Err(NOT_YET.into())
}

/// Where the password and the certificate are kept: the user's local application data, which only the user
/// (and the system's administrators) can read.
pub fn data_directory() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_default().join(crate::NAME)
}

pub fn password_path() -> PathBuf {
    data_directory().join("password")
}

/// Writes a file in the user's own directory, whose access it inherits, creating the directory.
pub fn write_private(path: &std::path::Path, contents: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(path.parent().unwrap())
        .and_then(|()| std::fs::write(path, contents))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// The machine's name, or nothing where it has none.
pub fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

/// Runs `action` with the console's echo off, if standard input is a console.
pub fn without_echo<T>(action: impl FnOnce() -> T) -> T {
    let console = unsafe { GetStdHandle(STD_INPUT_HANDLE) }.ok();
    let mut mode = CONSOLE_MODE::default();
    let console = console.filter(|&console| unsafe { GetConsoleMode(console, &mut mode) }.is_ok());
    if let Some(console) = console {
        let _ = unsafe { SetConsoleMode(console, mode & !ENABLE_ECHO_INPUT) };
    }
    let result = action();
    if let Some(console) = console {
        let _ = unsafe { SetConsoleMode(console, mode) };
    }
    result
}

pub fn screens() -> Result<Vec<Screen>, String> {
    not_yet()
}

/// Nothing is left changed in the desktop by a host here.
pub fn restore_leftovers() {}

pub mod capture {
    use super::*;
    use crate::capture::{Settings, Sink};

    pub fn run(_settings: Settings, _shared: Arc<Shared>, _sink: Sink) -> Result<(), String> {
        not_yet()
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

        pub fn text(&self, _text: &str) {}

        pub fn move_to(&self, _x: i32, _y: i32) {}

        pub fn scroll(&self, _horizontal: i32, _vertical: i32) {}
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
    use crate::screen::Window;

    pub const NAME: &str = crate::NAME;

    /// Windows makes no screen without a display driver of its own.
    pub struct VirtualScreen(());

    impl VirtualScreen {
        pub fn create(_window: Window, _fps: u32) -> Result<Self, String> {
            Err("Windows cannot make a screen of the client's size".into())
        }

        pub fn resize(&self, _window: Window) -> Result<(), String> {
            Ok(())
        }
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
    /// Showing the login screen needs a service of the system's, which is not built yet.
    pub fn run(_address: &str) -> ! {
        crate::fail(super::NOT_YET)
    }
}

pub mod tcp {
    use std::net::TcpStream;
    use std::time::Duration;

    use crate::tls::Control;

    /// Windows has no limit on what the kernel holds unsent for a connection.
    pub fn limit_unsent(_socket: &TcpStream, _bytes: i32) {}

    pub struct Path {
        pub round_trip: Duration,
        pub retransmitted: u32,
    }

    /// Not read yet: the connection keeps the congestion control it has.
    pub fn path(_socket: &TcpStream) -> Option<Path> {
        None
    }

    pub fn set_control(_socket: &TcpStream, _control: Control) {}
}
