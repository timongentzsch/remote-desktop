//! Clipboard text in both directions, through pasteboard.m. macOS counts every change of its clipboard, which
//! is looked at a few times a second.

use std::ffi::{c_char, c_long};
use std::io;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::capture::Shared;

/// Longer clipboard contents are not passed on, and how often the clipboard is looked at.
const LIMIT: usize = 1 << 20;
const LOOK: Duration = Duration::from_millis(100);

unsafe extern "C" {
    fn pasteboard_changes() -> c_long;
    fn pasteboard_read(out: *mut c_char, capacity: c_long) -> c_long;
    fn pasteboard_write(text: *const c_char, length: c_long);
}

/// Receives each new text on the desktop's clipboard.
pub type Copied = Box<dyn FnMut(String) + Send>;

/// Where the session leaves text the client copied, for the clipboard thread to pick up.
pub struct Inbox {
    text: Mutex<Option<String>>,
}

impl Inbox {
    pub fn new() -> io::Result<Arc<Self>> {
        Ok(Arc::new(Self { text: Mutex::new(None) }))
    }

    pub fn put(&self, text: String) {
        *self.text.lock().unwrap() = Some(text);
    }
}

/// Shares the clipboard until `shared.stop` is set.
pub fn run(inbox: &Inbox, shared: &Shared, mut copied: Copied) -> Result<(), String> {
    let mut seen = unsafe { pasteboard_changes() };
    let mut text = vec![0u8; LIMIT];
    while !shared.stop.load(Ordering::Relaxed) {
        if let Some(put) = inbox.text.lock().unwrap().take() {
            unsafe { pasteboard_write(put.as_ptr().cast(), put.len() as c_long) };
            seen = unsafe { pasteboard_changes() }; // not told back to the client it came from
        }
        let now = unsafe { pasteboard_changes() };
        if now != seen {
            seen = now;
            let length = unsafe { pasteboard_read(text.as_mut_ptr().cast(), LIMIT as c_long) };
            if length >= 0 {
                copied(String::from_utf8_lossy(&text[..length as usize]).into_owned());
            }
        }
        std::thread::sleep(LOOK);
    }
    Ok(())
}
