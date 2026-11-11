//! Clipboard text in both directions. One thread per session watches the desktop's clipboard and tells the
//! client when its text changes, and puts text the client copied onto the desktop's clipboard. How that is
//! done on a kind of desktop is a module here with one function, `run`.

mod wayland;
mod x11;

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::sync::{Arc, Mutex};

use crate::capture::Shared;
use crate::compositor;

/// Longer clipboard contents are not passed on.
const LIMIT: usize = 1 << 20;

/// Receives each new text on the desktop's clipboard.
pub type Copied = Box<dyn FnMut(String) + Send>;

/// Where the session leaves text the client copied, for the clipboard thread to pick up.
pub struct Inbox {
    text: Mutex<Option<String>>,
    wake: File,
}

impl Inbox {
    pub fn new() -> io::Result<Arc<Self>> {
        match unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) } {
            -1 => Err(io::Error::last_os_error()),
            fd => Ok(Arc::new(Self { text: Mutex::new(None), wake: unsafe { File::from_raw_fd(fd) } })),
        }
    }

    pub fn put(&self, text: String) {
        *self.text.lock().unwrap() = Some(text);
        let _ = (&self.wake).write_all(&1u64.to_ne_bytes());
    }

    /// Becomes readable when `put` was called, for the clipboard thread to wait on.
    fn waker(&self) -> RawFd {
        self.wake.as_raw_fd()
    }

    /// The text left since the last call, if any.
    fn take(&self) -> Option<String> {
        let _ = (&self.wake).read(&mut [0; 8]);
        self.text.lock().unwrap().take()
    }
}

/// Shares the clipboard until `shared.stop` is set.
pub fn run(inbox: &Inbox, shared: &Shared, copied: Copied) -> Result<(), String> {
    if compositor::available() { wayland::run(inbox, shared, copied) } else { x11::run(inbox, shared, copied) }
}
