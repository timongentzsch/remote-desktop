//! Clipboard text in both directions. One thread per session watches the desktop's clipboard and tells the
//! client when its text changes, and puts text the client copied onto the desktop's clipboard. How that is
//! done on a kind of desktop is a module here with one function, `run`.

mod mutter;
mod wayland;
mod x11;

use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::capture::Shared;
use crate::platform::linux::compositor;
use crate::platform::unix::fd::{self, Waker};

/// Longer clipboard contents are not passed on.
const LIMIT: usize = 1 << 20;
/// How long an application may leave the pipe without a word before it is given up on, and how much is read
/// from it at a time.
const STALL: Duration = Duration::from_secs(1);
const CHUNK: usize = 1 << 16;
/// How often the clipboard thread looks whether the session has ended, between the desktop's events.
pub const LOOK: Duration = Duration::from_millis(250);
/// The names text goes by on a Wayland clipboard, most precise first.
const TEXT_TYPES: [&str; 4] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "STRING"];

/// Receives each new text on the desktop's clipboard.
pub type Copied = Box<dyn FnMut(String) + Send>;

/// Where the session leaves text the client copied, for the clipboard thread to pick up.
pub struct Inbox {
    text: Mutex<Option<String>>,
    waker: Waker,
}

impl Inbox {
    pub fn new() -> io::Result<Arc<Self>> {
        Ok(Arc::new(Self { text: Mutex::new(None), waker: Waker::new()? }))
    }

    pub fn put(&self, text: String) {
        *self.text.lock().unwrap() = Some(text);
        self.waker.wake();
    }

    /// Becomes readable when `put` was called, for the clipboard thread to wait on.
    fn waker(&self) -> RawFd {
        self.waker.fd()
    }

    /// The text left since the last call, if any.
    fn take(&self) -> Option<String> {
        self.waker.settle();
        self.text.lock().unwrap().take()
    }
}

/// Shares the clipboard until `shared.stop` is set.
pub fn run(inbox: &Inbox, shared: &Shared, copied: Copied) -> Result<(), String> {
    if !compositor::available() {
        return x11::run(inbox, shared, copied);
    }
    let offered = compositor::protocols()?;
    if wayland::offered(&offered) { wayland::run(inbox, shared, copied) } else { mutter::run(inbox, shared, copied) }
}

/// Reads the text an application writes into a pipe. One that stalls, or writes too much, is given up on.
fn read_text(mut pipe: File) -> Option<String> {
    let mut text = Vec::new();
    let mut chunk = [0; CHUNK];
    loop {
        if fd::wait_readable([pipe.as_raw_fd()], Some(STALL)) == [false] {
            return None;
        }
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) if text.len() + count <= LIMIT => text.extend(&chunk[..count]),
            _ => return None,
        }
    }
    Some(String::from_utf8_lossy(&text).into_owned())
}
