//! macOS: the picture and the sound through ScreenCaptureKit (capture.rs, audio.rs), keys and the pointer as
//! CGEvents (input.rs), the clipboard (clipboard.rs), a virtual display for a client's size
//! (virtual_screen.rs), and a LaunchAgent (install.rs).

use std::path::PathBuf;
use std::sync::atomic::Ordering;

pub use super::unix::{fd, hostname, without_echo, write_private};
use crate::screen::Screen;

/// Where the password and the certificate are kept: the user's Application Support.
pub fn data_directory() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join("Library/Application Support").join(crate::NAME)
}

pub mod audio;
pub mod capture;
pub mod clipboard;
pub mod input;
pub mod install;
pub mod virtual_screen;

#[repr(C)]
struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// The most displays macOS drives at once that are listed.
const MOST_DISPLAYS: usize = 16;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetActiveDisplayList(most: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayBounds(display: u32) -> Rect;
    fn CGDisplayMirrorsDisplay(display: u32) -> u32;
}

/// The displays that show a picture of their own: not those that mirror another.
fn displays() -> Vec<u32> {
    let (mut ids, mut count) = ([0; MOST_DISPLAYS], 0);
    unsafe { CGGetActiveDisplayList(MOST_DISPLAYS as u32, ids.as_mut_ptr(), &mut count) };
    ids[..count as usize].iter().copied().filter(|&id| unsafe { CGDisplayMirrorsDisplay(id) } == 0).collect()
}

/// The displays, each named by its number but the virtual one (see virtual_screen.rs), where they lie in points.
pub fn screens() -> Result<Vec<Screen>, String> {
    let screen = |id| {
        let bounds = unsafe { CGDisplayBounds(id) };
        let point = |value: f64| value as i32;
        Screen {
            name: if id == virtual_screen::DISPLAY.load(Ordering::SeqCst) {
                virtual_screen::NAME.to_owned()
            } else {
                id.to_string()
            },
            x: point(bounds.x),
            y: point(bounds.y),
            width: point(bounds.width),
            height: point(bounds.height),
        }
    };
    Ok(displays().into_iter().map(screen).collect())
}

/// The number of the display a session asked for by name, else the first. The virtual display's is 0 while it is
/// being made; where there is none, the first stands in for it.
fn display(wanted: Option<&str>) -> Result<u32, String> {
    if wanted == Some(virtual_screen::NAME) && virtual_screen::in_use() {
        return Ok(virtual_screen::DISPLAY.load(Ordering::SeqCst));
    }
    let screens = screens()?;
    let chosen = crate::screen::choose(&screens, wanted).ok_or("there is no display")?;
    Ok(chosen.name.parse().unwrap_or_default())
}

/// launchd keeps what the host says (see install.rs).
pub fn serve_quietly() {}

pub mod greeter {
    /// macOS lets no program show its login window.
    pub fn run(_address: &str) -> ! {
        crate::fail("there is no login screen to show on macOS")
    }

    pub fn stand_back() {}
}
