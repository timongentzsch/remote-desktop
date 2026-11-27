//! macOS: the picture and the sound through ScreenCaptureKit (capture.rs, audio.rs), keys and the pointer as
//! CGEvents (input.rs), the clipboard (clipboard.rs), a virtual display for a client's size
//! (virtual_screen.rs), and a LaunchAgent (install.rs).

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

pub use super::unix::{fd, hostname, serve_quietly, without_echo, write_private};
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
    fn CGDisplayRegisterReconfigurationCallback(told: extern "C" fn(u32, u32, *mut c_void), with: *mut c_void) -> i32;
    fn CFRunLoopRun();
}

/// Serves for good, on another thread: this one, the program's first, runs its run loop. macOS tells a program
/// through that loop which displays have come and gone, and without it the list of displays here stopped
/// following them once a virtual display was made a second time (macOS 27): the list kept one that had gone,
/// and no session found a display to capture until the host was started again. Asking to be told of such
/// changes is what gives the loop something to wait for; nothing is done when told.
pub fn run_main(serve: impl FnOnce() + Send + 'static) -> ! {
    extern "C" fn told(_display: u32, _change: u32, _with: *mut c_void) {}
    std::thread::spawn(serve);
    unsafe {
        CGDisplayRegisterReconfigurationCallback(told, std::ptr::null_mut());
        CFRunLoopRun();
    }
    loop {
        std::thread::park();
    }
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
        Screen {
            name: if id == virtual_screen::DISPLAY.load(Ordering::SeqCst) {
                virtual_screen::NAME.to_owned()
            } else {
                id.to_string()
            },
            x: bounds.x as i32,
            y: bounds.y as i32,
            width: bounds.width as i32,
            height: bounds.height as i32,
        }
    };
    Ok(displays().into_iter().map(screen).collect())
}

/// The number of the display a session asked for by name, else the first. The virtual display's is 0 while it is
/// being made; where there is none, the first stands in for it.
fn display(wanted: Option<&str>) -> Result<u32, String> {
    let made = virtual_screen::DISPLAY.load(Ordering::SeqCst);
    if wanted == Some(virtual_screen::NAME) && virtual_screen::in_use() {
        return Ok(made);
    }
    let screens = screens()?;
    let chosen = crate::screen::choose(&screens, wanted).ok_or("there is no display")?;
    // Every display is named by its number but the virtual one, which another session's may be the first.
    Ok(chosen.name.parse().unwrap_or(made))
}
