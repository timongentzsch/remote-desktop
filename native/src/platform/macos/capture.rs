//! The screen's picture through ScreenCaptureKit, which capture.m drives: it hands over each new frame of the
//! chosen display, and none while the screen is still.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::Arc;

use crate::capture::{Settings, Shared, Sink};
use crate::stream::Stream;

const ERROR_SIZE: usize = 256;

unsafe extern "C" {
    fn capture_start(display: u32, fps: c_int, cursor: c_int, error: *mut c_char, size: c_int) -> *mut c_void;
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

/// Captures the chosen screen until the stream is no longer wanted.
pub fn run(settings: Settings, shared: Arc<Shared>, sink: Sink) -> Result<(), String> {
    let display = super::display(settings.screen.as_deref())?;
    let mut stream = Stream::new(settings, shared, sink);
    let mut error = [0 as c_char; ERROR_SIZE];
    let fps = stream.settings.fps as c_int;
    let capture = unsafe { capture_start(display, fps, stream.settings.cursor as c_int, error.as_mut_ptr(), ERROR_SIZE as c_int) };
    if capture.is_null() {
        let reason = unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy();
        return Err(format!("the screen cannot be captured: {reason}"));
    }
    let mut ended = Ok(());
    while stream.wanted() {
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
    ended.and(stream.ended.take().unwrap_or(Ok(())))
}
