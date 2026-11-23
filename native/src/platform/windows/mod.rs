//! Windows: the picture through Desktop Duplication (capture.rs), keys and the pointer through SendInput
//! (input.rs), sound through WASAPI (audio.rs), the clipboard (clipboard.rs), and a host started at logon from
//! the user's Run key (install.rs). The login screen is not built yet, and says so.

use std::fs::File;
use std::os::windows::io::IntoRawHandle;
use std::path::PathBuf;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput};
use windows::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_ECHO_INPUT, FreeConsole, GetConsoleMode, GetConsoleProcessList, GetStdHandle,
    STD_ERROR_HANDLE, STD_INPUT_HANDLE, SetConsoleMode, SetStdHandle,
};

use crate::screen::Screen;

pub mod audio;
pub mod capture;
pub mod clipboard;
pub mod fd;
pub mod input;
pub mod install;

const NOT_YET: &str = "this part of the host is not built for Windows yet";

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

/// A host started at logon or by `install` has no console, or one opened for it alone, whose window it closes:
/// what it says goes to a log beside its data instead. Started from a terminal, it goes on saying it there.
pub fn serve_quietly() {
    let mut processes = [0u32; 2];
    if unsafe { GetConsoleProcessList(&mut processes) } > 1 {
        return;
    }
    let _ = unsafe { FreeConsole() };
    let log = data_directory().join(format!("{}.log", crate::NAME));
    if let Ok(file) = std::fs::create_dir_all(data_directory()).and_then(|()| File::create(log)) {
        let _ = unsafe { SetStdHandle(STD_ERROR_HANDLE, HANDLE(file.into_raw_handle())) };
    }
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

/// The displays that are part of the desktop, each with the graphics adapter that drives it, in the order
/// Windows lists them: the main display first.
fn outputs() -> windows::core::Result<Vec<(Screen, IDXGIAdapter1, IDXGIOutput)>> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;
    let mut outputs = Vec::new();
    for adapter in (0..).map_while(|index| unsafe { factory.EnumAdapters1(index) }.ok()) {
        for output in (0..).map_while(|index| unsafe { adapter.EnumOutputs(index) }.ok()) {
            let description = unsafe { output.GetDesc() }?;
            if !description.AttachedToDesktop.as_bool() {
                continue;
            }
            let name = String::from_utf16_lossy(&description.DeviceName);
            let bounds = description.DesktopCoordinates;
            let screen = Screen {
                // \\.\DISPLAY1 is called DISPLAY1.
                name: name.trim_end_matches('\0').trim_start_matches(r"\\.\").to_owned(),
                x: bounds.left,
                y: bounds.top,
                width: bounds.right - bounds.left,
                height: bounds.bottom - bounds.top,
            };
            outputs.push((screen, adapter.clone(), output));
        }
    }
    Ok(outputs)
}

/// The displays, named as Windows names them, where they lie on the desktop in pixels.
pub fn screens() -> Result<Vec<Screen>, String> {
    let outputs = outputs().map_err(|error| format!("the displays cannot be listed: {error}"))?;
    Ok(outputs.into_iter().map(|(screen, ..)| screen).collect())
}

/// Nothing is left changed in the desktop by a host here.
pub fn restore_leftovers() {}

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
