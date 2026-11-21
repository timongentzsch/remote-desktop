//! Clipboard text in both directions. Windows counts every change of its clipboard, which is looked at a few
//! times a second; text the client copied is put there through a window of the host's own, which nobody sees,
//! as only a window may own what it puts on the clipboard.

use std::io;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, IsClipboardFormatAvailable,
    OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
};
use windows::core::w;

use crate::capture::Shared;

/// CF_UNICODETEXT: text as UTF-16, ending with a zero.
const TEXT: u32 = 13;
/// Longer clipboard contents are not passed on, and how often the clipboard is looked at.
const LIMIT: usize = 1 << 20;
const LOOK: Duration = Duration::from_millis(100);

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
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            None,
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        )
    }
    .map_err(|error| format!("the clipboard cannot be shared: {error}"))?;
    let mut seen = unsafe { GetClipboardSequenceNumber() };
    while !shared.stop.load(Ordering::Relaxed) {
        if let Some(text) = inbox.text.lock().unwrap().take() {
            put(window, &text);
            seen = unsafe { GetClipboardSequenceNumber() }; // not told back to the client it came from
        }
        let now = unsafe { GetClipboardSequenceNumber() };
        if now != seen {
            seen = now;
            if let Some(text) = read(window) {
                copied(text);
            }
        }
        std::thread::sleep(LOOK);
    }
    let _ = unsafe { DestroyWindow(window) };
    Ok(())
}

/// The clipboard's text, if it has some and it is not too long.
fn read(window: HWND) -> Option<String> {
    unsafe { IsClipboardFormatAvailable(TEXT) }.ok()?;
    unsafe { OpenClipboard(Some(window)) }.ok()?;
    let text = (|| {
        let memory = HGLOBAL(unsafe { GetClipboardData(TEXT) }.ok()?.0);
        let units = (unsafe { GlobalSize(memory) } / 2).min(LIMIT + 1);
        let pointer = unsafe { GlobalLock(memory) }.cast::<u16>();
        if pointer.is_null() {
            return None;
        }
        let all = unsafe { std::slice::from_raw_parts(pointer, units) };
        let text = all.iter().position(|&unit| unit == 0).map(|end| String::from_utf16_lossy(&all[..end]));
        let _ = unsafe { GlobalUnlock(memory) };
        text.filter(|text| text.len() <= LIMIT)
    })();
    let _ = unsafe { CloseClipboard() };
    text
}

/// Puts `text` on the clipboard, owned by `window`.
fn put(window: HWND, text: &str) {
    let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let Ok(memory) = (unsafe { GlobalAlloc(GMEM_MOVEABLE, units.len() * 2) }) else { return };
    let pointer = unsafe { GlobalLock(memory) }.cast::<u16>();
    if pointer.is_null() {
        let _ = unsafe { GlobalFree(Some(memory)) };
        return;
    }
    unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), pointer, units.len()) };
    let _ = unsafe { GlobalUnlock(memory) };
    if unsafe { OpenClipboard(Some(window)) }.is_err() {
        let _ = unsafe { GlobalFree(Some(memory)) };
        return;
    }
    let _ = unsafe { EmptyClipboard() };
    // Once on the clipboard, the memory is the system's.
    if unsafe { SetClipboardData(TEXT, Some(HANDLE(memory.0))) }.is_err() {
        let _ = unsafe { GlobalFree(Some(memory)) };
    }
    let _ = unsafe { CloseClipboard() };
}
