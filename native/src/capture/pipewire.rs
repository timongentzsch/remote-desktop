//! Pictures of the screen from a PipeWire screen-cast stream, through the C wrapper in pipewire_stream.c.
//! GNOME and KDE both hand out the screen this way, each after being asked in its own manner.

use std::ffi::c_int;
use std::time::Duration;
use std::{ptr, slice};

use crate::stream::Stream;

/// Feeds the pictures of PipeWire node `node` to the stream until the stream is no longer wanted or the
/// desktop, named for the error message, ends the cast.
pub fn run(node: u32, mut stream: Stream, desktop: &str) -> Result<(), String> {
    let mut screen = Screen::open(node, stream.settings.fps).ok_or("could not receive the screen from PipeWire")?;
    while stream.wanted() {
        match screen.next(stream.patience()) {
            Ok(Some(picture)) => stream.picture(picture.pixels, picture.stride, picture.width, picture.height),
            Ok(None) => {}
            Err(()) => return Err(format!("{desktop} ended the screen cast")),
        }
        stream.refine();
    }
    stream.ended.take().unwrap_or(Ok(()))
}

#[repr(C)]
struct Raw {
    _opaque: [u8; 0],
}

unsafe extern "C" {
    fn screen_open(node: u32, fps: c_int) -> *mut Raw;
    fn screen_next(
        screen: *mut Raw,
        timeout_ms: c_int,
        pixels: *mut *const u8,
        stride: *mut c_int,
        width: *mut c_int,
        height: *mut c_int,
    ) -> c_int;
    fn screen_close(screen: *mut Raw);
}

/// One picture of the screen: rows of pixels `stride` bytes apart, each blue, green, red and a fourth byte.
struct Picture<'a> {
    pixels: &'a [u8],
    stride: usize,
    width: usize,
    height: usize,
}

struct Screen(*mut Raw);

impl Screen {
    /// Starts receiving the stream of PipeWire node `node`, at no more than `fps` pictures a second.
    /// None if PipeWire is missing or refuses.
    fn open(node: u32, fps: u32) -> Option<Self> {
        let raw = unsafe { screen_open(node, fps as c_int) };
        (!raw.is_null()).then(|| Self(raw))
    }

    /// Waits up to `wait` for the screen to change. None if it did not; an error once the stream has ended.
    fn next(&mut self, wait: Duration) -> Result<Option<Picture<'_>>, ()> {
        let mut pixels = ptr::null();
        let (mut stride, mut width, mut height) = (0, 0, 0);
        match unsafe {
            screen_next(self.0, wait.as_millis() as c_int, &mut pixels, &mut stride, &mut width, &mut height)
        } {
            1 => {
                let (stride, width, height) = (stride as usize, width as usize, height as usize);
                // The picture stays valid until the next call, which needs `&mut self`.
                Ok(Some(Picture {
                    pixels: unsafe { slice::from_raw_parts(pixels, stride * height) },
                    stride,
                    width,
                    height,
                }))
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
