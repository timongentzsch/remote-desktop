//! Windows: the picture through Desktop Duplication (capture.rs), keys and the pointer through SendInput
//! (input.rs), sound through WASAPI (audio.rs), the clipboard (clipboard.rs), and a host started at logon from
//! the user's Run key (install.rs). The login screen is not built yet, and says so.

use std::fs::File;
use std::os::windows::io::IntoRawHandle;
use std::path::PathBuf;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput};
use windows::Win32::Graphics::Gdi::{
    DEVMODEW, DM_PELSHEIGHT, DM_PELSWIDTH, DM_POSITION, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE,
    EnumDisplaySettingsExW,
};
use windows::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_ECHO_INPUT, FreeConsole, GetConsoleMode, GetConsoleProcessList, GetStdHandle,
    STD_ERROR_HANDLE, STD_INPUT_HANDLE, SetConsoleMode, SetStdHandle,
};
use windows::core::PCWSTR;

use crate::screen::Screen;

pub mod audio;
pub mod capture;
pub mod clipboard;
pub mod fd;
pub mod input;
pub mod install;
pub mod virtual_screen;

const NOT_YET: &str = "this part of the host is not built for Windows yet";

/// Where the password and the certificate are kept: the user's local application data, which only the user
/// (and the system's administrators) can read.
pub fn data_directory() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_default().join(crate::NAME)
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
            // DXGI can virtualize DesktopCoordinates at high DPI. The current display mode always
            // reports physical pixels, including each monitor's offset in the virtual desktop.
            let device = PCWSTR(description.DeviceName.as_ptr());
            let (x, y, width, height) = if let Some(mode) = display_mode(device, ENUM_CURRENT_SETTINGS)
                .filter(|m| m.dmFields.contains(DM_POSITION | DM_PELSWIDTH | DM_PELSHEIGHT))
            {
                let origin = unsafe { mode.Anonymous1.Anonymous2.dmPosition };
                (origin.x, origin.y, mode.dmPelsWidth as i32, mode.dmPelsHeight as i32)
            } else {
                // A driver without display-mode metadata can still provide a usable DXGI output.
                let rect = description.DesktopCoordinates;
                (rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top)
            };
            let screen = Screen {
                // \\.\DISPLAY1 is called DISPLAY1.
                name: name.trim_end_matches('\0').trim_start_matches(r"\\.\").to_owned(),
                x,
                y,
                width,
                height,
            };
            outputs.push((screen, adapter.clone(), output));
        }
    }
    Ok(outputs)
}

fn display_mode(device: PCWSTR, index: ENUM_DISPLAY_SETTINGS_MODE) -> Option<DEVMODEW> {
    let mut mode = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    unsafe { EnumDisplaySettingsExW(device, index, &mut mode, Default::default()) }.as_bool().then_some(mode)
}

/// The displays, named as Windows names them, where they lie on the desktop in pixels.
pub fn screens() -> Result<Vec<Screen>, String> {
    let outputs = outputs().map_err(|error| format!("the displays cannot be listed: {error}"))?;
    Ok(outputs.into_iter().map(|(screen, ..)| screen).collect())
}

pub mod greeter {
    /// Showing the login screen needs a service of the system's, which is not built yet.
    pub fn run(_address: &str) -> ! {
        crate::fail(super::NOT_YET)
    }
}
