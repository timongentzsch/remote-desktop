//! A display made to the size of the client's window, for a client that asked to match its device (see
//! virtual_screen.m). The Mac's own displays mirror it meanwhile, so that what is on them is on it, at its size.
//!
//! macOS keeps such a display, at the size it was made, for as long as the process that made it. So a helper
//! makes it: this program again, as `remote-desktop virtual-display WIDTH HEIGHT SCALE FPS`, which says the
//! display's number and keeps it until its standard input ends. Another size is another helper.

use std::ffi::{CString, c_void};
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::screen::Window;

/// What it is called, as the capture finds it (see `screens`).
pub const NAME: &str = crate::NAME;
/// The helper's command.
pub const HELPER: &str = "virtual-display";
/// Its number while it is there, 0 otherwise, and its size: points across and down, and pixels to a point. This
/// process cannot read the pixels of a display that came after it started (see capture.m), so it keeps them.
pub static DISPLAY: AtomicU32 = AtomicU32::new(0);
pub static SIZE: Mutex<[u32; 3]> = Mutex::new([0; 3]);
/// One at a time: a session's is gone, and the Mac's own displays back, before the next one makes its own.
static IN_USE: AtomicBool = AtomicBool::new(false);

/// Whether a session has the virtual display, or is making it.
pub fn in_use() -> bool {
    IN_USE.load(Ordering::SeqCst)
}
/// macOS draws one or two pixels to a point: two from this scale on.
const HIDPI_FROM: f64 = 1.5;
/// How long the display may take to appear, and to go, as when a session ends and the next one wants it.
const APPEARING: Duration = Duration::from_secs(2);
const HANDOVER: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);
/// Display changes that last as long as the user's session, not beyond.
const FOR_SESSION: u32 = 1;

unsafe extern "C" {
    fn virtual_display_create(
        name: *const i8,
        width: u32,
        height: u32,
        scale: u32,
        fps: u32,
        id: *mut u32,
    ) -> *mut c_void;
    fn CGBeginDisplayConfiguration(config: *mut *mut c_void) -> i32;
    fn CGConfigureDisplayMirrorOfDisplay(config: *mut c_void, display: u32, master: u32) -> i32;
    fn CGCompleteDisplayConfiguration(config: *mut c_void, option: u32) -> i32;
}

/// The virtual screen while it is there: its helper, at the size it is now. Dropping it takes it away.
pub struct VirtualScreen {
    made: Mutex<Made>,
    fps: u32,
}

/// The helper, the display's number, and the size it made it at: points across and down, and pixels to a point.
struct Made {
    helper: Option<(Child, u32)>,
    size: [u32; 3],
}

fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let since = Instant::now();
    while !done() {
        if since.elapsed() > limit {
            return false;
        }
        thread::sleep(POLL);
    }
    true
}

/// The display's size for `window`.
fn size(window: Window) -> [u32; 3] {
    let (width, height) = window.pixels();
    let scale = if window.scale() >= HIDPI_FROM { 2 } else { 1 };
    [width / scale, height / scale, scale]
}

/// Starts a helper with the display of `size`, has the Mac's own displays mirror it, and notes its number.
fn make(size: [u32; 3], fps: u32) -> Result<(Child, u32), String> {
    let program = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut helper = Command::new(program)
        .arg(HELPER)
        .args(size.iter().chain([&fps]).map(u32::to_string))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot start the virtual display's helper: {error}"))?;
    let mut answer = String::new();
    let _ = BufReader::new(helper.stdout.take().unwrap()).read_line(&mut answer);
    let Ok(id @ 1..) = answer.trim().parse::<u32>() else {
        end(helper, 0);
        let [width, height, scale] = size;
        return Err(format!("macOS made no virtual display of {width}x{height} points at {scale}x: {answer:?}"));
    };
    if !wait_for(APPEARING, || super::displays().contains(&id)) {
        end(helper, id);
        return Err("the virtual display did not appear".into());
    }
    // A Mac without a monitor has a stand-in display, which goes once there is another. The others mirror it.
    let own: Vec<u32> = super::displays().into_iter().filter(|&other| other != id).collect();
    let mut config = std::ptr::null_mut();
    unsafe {
        CGBeginDisplayConfiguration(&mut config);
        for display in own {
            CGConfigureDisplayMirrorOfDisplay(config, display, id);
        }
        CGCompleteDisplayConfiguration(config, FOR_SESSION);
    }
    *SIZE.lock().unwrap() = size;
    DISPLAY.store(id, Ordering::SeqCst);
    Ok((helper, id))
}

/// Ends a helper, and with it display `id`; returns once that is gone and the Mac's own displays are back.
fn end(mut helper: Child, id: u32) {
    let _ = DISPLAY.compare_exchange(id, 0, Ordering::SeqCst, Ordering::SeqCst);
    let _ = helper.kill();
    let _ = helper.wait();
    wait_for(HANDOVER, || {
        let displays = super::displays();
        !displays.contains(&id) && !displays.is_empty()
    });
}

impl VirtualScreen {
    /// Makes the display for `window` at `fps` frames a second.
    pub fn create(window: Window, fps: u32) -> Result<Self, String> {
        let taken = || IN_USE.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok();
        if !wait_for(HANDOVER, taken) {
            return Err("another client has the virtual screen".into());
        }
        // From here on, dropping it gives it back, also after a failure.
        let made = Self { made: Mutex::new(Made { helper: None, size: size(window) }), fps };
        let helper = make(size(window), fps)?;
        made.made.lock().unwrap().helper = Some(helper);
        Ok(made)
    }

    /// Makes the display anew where the client's window has changed size.
    pub fn resize(&self, window: Window) -> Result<(), String> {
        let mut made = self.made.lock().unwrap();
        if made.size == size(window) {
            return Ok(());
        }
        made.end();
        made.size = size(window);
        made.helper = Some(make(made.size, self.fps)?);
        Ok(())
    }
}

impl Made {
    fn end(&mut self) {
        if let Some((helper, id)) = self.helper.take() {
            end(helper, id);
        }
    }
}

impl Drop for VirtualScreen {
    fn drop(&mut self) {
        self.made.get_mut().unwrap().end();
        IN_USE.store(false, Ordering::SeqCst);
    }
}

/// `remote-desktop virtual-display WIDTH HEIGHT SCALE FPS`: makes the display, says its number (0 if it could
/// not), and keeps it until standard input ends.
pub fn helper(arguments: impl Iterator<Item = String>) {
    let numbers: Vec<u32> = arguments.filter_map(|number| number.parse().ok()).collect();
    let &[width, height, scale, fps] = numbers.as_slice() else { return println!("0") };
    let name = CString::new(NAME).unwrap();
    let mut id = 0;
    let display = unsafe { virtual_display_create(name.as_ptr(), width, height, scale, fps, &mut id) };
    println!("{}", if display.is_null() { 0 } else { id });
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
}
