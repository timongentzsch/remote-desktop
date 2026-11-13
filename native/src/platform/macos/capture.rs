//! The screen's picture through ScreenCaptureKit, which capture.m drives: it hands over each new frame of the
//! chosen display, and none while the screen is still. A display that changes size is captured anew.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::virtual_screen::{self, DISPLAY};
use crate::capture::{Settings, Shared, Sink};
use crate::stream::Stream;

const ERROR_SIZE: usize = 256;

unsafe extern "C" {
    fn display_pixels(display: u32, width: *mut usize, height: *mut usize);
    fn capture_start(
        display: u32,
        width: usize,
        height: usize,
        fps: c_int,
        cursor: c_int,
        error: *mut c_char,
        size: c_int,
    ) -> *mut c_void;
    fn capture_next(
        capture: *mut c_void,
        millis: c_int,
        frame: *mut *mut c_void,
        pixels: *mut *const u8,
        stride: *mut usize,
        width: *mut usize,
        height: *mut usize,
    ) -> c_int;
    fn capture_release(frame: *mut c_void);
    fn capture_stop(capture: *mut c_void);
}

/// The display's size in pixels: the virtual display's as it was made (see virtual_screen.rs).
fn pixels(display: u32) -> (usize, usize) {
    if display != 0 && display == DISPLAY.load(Ordering::SeqCst) {
        let [width, height, scale] = *virtual_screen::SIZE.lock().unwrap();
        return ((width * scale) as usize, (height * scale) as usize);
    }
    let (mut width, mut height) = (0, 0);
    unsafe { display_pixels(display, &mut width, &mut height) };
    (width, height)
}

/// How long the displays may refuse to be captured: as one just made does at first, and while a virtual display
/// goes and the Mac's own come back (see virtual_screen.rs).
const REFUSING: Duration = Duration::from_secs(8);

/// Captures the chosen screen until the stream is no longer wanted.
pub fn run(settings: Settings, shared: Arc<Shared>, sink: Sink) -> Result<(), String> {
    let mut stream = Stream::new(settings, shared, sink);
    let mut refused = None;
    while stream.wanted() {
        match capture(&mut stream) {
            Ok(()) => refused = None,
            Err(error) if refused.get_or_insert_with(Instant::now).elapsed() > REFUSING => return Err(error),
            Err(_) => std::thread::sleep(stream.frame_interval()),
        }
    }
    stream.ended.take().unwrap_or(Ok(()))
}

/// Captures the chosen display at its size, until another is chosen, its size changes, or the stream is no longer
/// wanted.
fn capture(stream: &mut Stream) -> Result<(), String> {
    let display = super::display(stream.settings.screen.as_deref())?;
    let size = pixels(display);
    // A display being made has no number or no size yet.
    if size == (0, 0) {
        std::thread::sleep(stream.frame_interval());
        return Ok(());
    }
    let mut error = [0 as c_char; ERROR_SIZE];
    let (fps, cursor) = (stream.settings.fps as c_int, stream.settings.cursor as c_int);
    let capture =
        unsafe { capture_start(display, size.0, size.1, fps, cursor, error.as_mut_ptr(), ERROR_SIZE as c_int) };
    if capture.is_null() {
        let reason = unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy();
        return Err(format!("the screen cannot be captured: {reason}"));
    }
    let mut ended = Ok(());
    let unchanged =
        |stream: &Stream| super::display(stream.settings.screen.as_deref()) == Ok(display) && pixels(display) == size;
    while stream.wanted() && unchanged(stream) {
        let millis = stream.patience().as_millis().min(c_int::MAX as u128) as c_int;
        let (mut frame, mut pixels, mut stride, mut width, mut height) =
            (std::ptr::null_mut(), std::ptr::null(), 0, 0, 0);
        match unsafe { capture_next(capture, millis, &mut frame, &mut pixels, &mut stride, &mut width, &mut height) } {
            1 => {
                let bytes = unsafe { std::slice::from_raw_parts(pixels, stride * height) };
                stream.picture(bytes, stride, width, height);
                unsafe { capture_release(frame) };
            }
            0 => {}
            _ => {
                ended = Err("the screen's capture stopped".into());
                break;
            }
        }
        stream.refine();
    }
    unsafe { capture_stop(capture) };
    ended
}
