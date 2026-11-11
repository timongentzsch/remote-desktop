//! Pictures of the screen from a PipeWire screen-cast stream, through the C wrapper in pipewire_stream.c.

use std::ffi::c_int;
use std::time::Duration;
use std::{ptr, slice};

#[repr(C)]
struct Raw {
    _opaque: [u8; 0],
}

unsafe extern "C" {
    fn screen_open(node: u32, fps: c_int) -> *mut Raw;
    fn screen_next(screen: *mut Raw, timeout_ms: c_int, pixels: *mut *const u8, stride: *mut c_int, width: *mut c_int, height: *mut c_int) -> c_int;
    fn screen_close(screen: *mut Raw);
}

/// One picture of the screen: rows of pixels `stride` bytes apart, each blue, green, red and a fourth byte.
pub struct Picture<'a> {
    pub pixels: &'a [u8],
    pub stride: usize,
    pub width: usize,
    pub height: usize,
}

pub struct Screen(*mut Raw);

impl Screen {
    /// Starts receiving the stream of PipeWire node `node`, at no more than `fps` pictures a second.
    /// None if PipeWire is missing or refuses.
    pub fn open(node: u32, fps: u32) -> Option<Self> {
        let raw = unsafe { screen_open(node, fps as c_int) };
        (!raw.is_null()).then(|| Self(raw))
    }

    /// Waits up to `wait` for the screen to change. None if it did not; an error once the stream has ended.
    pub fn next(&mut self, wait: Duration) -> Result<Option<Picture<'_>>, ()> {
        let mut pixels = ptr::null();
        let (mut stride, mut width, mut height) = (0, 0, 0);
        match unsafe { screen_next(self.0, wait.as_millis() as c_int, &mut pixels, &mut stride, &mut width, &mut height) } {
            1 => {
                let (stride, width, height) = (stride as usize, width as usize, height as usize);
                // The picture stays valid until the next call, which needs `&mut self`.
                Ok(Some(Picture { pixels: unsafe { slice::from_raw_parts(pixels, stride * height) }, stride, width, height }))
            }
            0 => Ok(None),
            _ => Err(()),
        }
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        unsafe { screen_close(self.0) }
    }
}
