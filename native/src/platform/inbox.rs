//! Where a session leaves text the client copied, on the systems whose clipboard is looked at a few times a
//! second, macOS and Windows: the clipboard's thread picks it up at its next look.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Longer clipboard contents are not passed on, and how often the clipboard is looked at.
pub const LIMIT: usize = 1 << 20;
pub const LOOK: Duration = Duration::from_millis(100);

/// Receives each new text on the desktop's clipboard.
pub type Copied = Box<dyn FnMut(String) + Send>;

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

    pub fn take(&self) -> Option<String> {
        self.text.lock().unwrap().take()
    }
}
