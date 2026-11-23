//! Keys and the pointer, sent with SendInput. Keys come as Linux's evdev codes, which for the main keys are
//! the PC keyboard's scan codes themselves; the others are scan codes after the E0 prefix ("extended"). The
//! pointer comes in the range of the box around all screens (see screen.rs), which is the range Windows takes
//! absolute positions on its virtual desktop in. Text is typed as Unicode characters.

use std::io;
use std::sync::Mutex;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
};

use crate::typing;

/// The evdev codes of the pointer's buttons, left, right, middle, back and forward, as index.html sends them,
/// and what Windows calls each going down and up, with the extra buttons' numbers.
const BTN_LEFT: u16 = 0x110;
const BUTTONS: [(MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS, i32); 5] = [
    (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
    (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
    (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
    (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 1),
    (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 2),
];
/// The evdev codes up to which a key's code is its scan code: Escape (1) to F12 (88).
const PLAIN: u16 = 88;
/// The keys whose scan codes come after E0, by evdev code: keypad Enter, right Control, keypad /, Print
/// Screen, right Alt, Home, Up, Page Up, Left, Right, End, Down, Page Down, Insert, Delete, the Windows keys and
/// the menu key.
const EXTENDED: [(u16, u16); 18] = [
    (96, 0x1C),
    (97, 0x1D),
    (98, 0x35),
    (99, 0x37),
    (100, 0x38),
    (102, 0x47),
    (103, 0x48),
    (104, 0x49),
    (105, 0x4B),
    (106, 0x4D),
    (107, 0x4F),
    (108, 0x50),
    (109, 0x51),
    (110, 0x52),
    (111, 0x53),
    (125, 0x5B),
    (126, 0x5C),
    (127, 0x5D),
];
/// The modifiers that make a shortcut of a character, by evdev code: the Control, Alt and Windows keys.
const SHORTCUT_MODIFIERS: [u16; 6] = [29, 97, 56, 100, 125, 126];

pub struct Input {
    /// The shortcut modifiers held, a bit each in the order of SHORTCUT_MODIFIERS.
    held: Mutex<u8>,
}

/// The scan code of the key with evdev code `code`, and whether it is extended.
fn scan_code(code: u16) -> Option<(u16, bool)> {
    if (1..=PLAIN).contains(&code) {
        return Some((code, false));
    }
    EXTENDED.iter().find(|(evdev, _)| *evdev == code).map(|&(_, scan)| (scan, true))
}

fn keyboard(scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    let key = KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 };
    INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: key } }
}

fn mouse(flags: MOUSE_EVENT_FLAGS, x: i32, y: i32, data: i32) -> INPUT {
    let pointer = MOUSEINPUT { dx: x, dy: y, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 };
    INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi: pointer } }
}

fn send(inputs: &[INPUT]) {
    unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
}

impl Input {
    pub fn open() -> io::Result<Self> {
        Ok(Self { held: Mutex::new(0) })
    }

    pub fn key(&self, code: u16, down: bool) {
        if let Some(&(pressed, released, which)) = BUTTONS.get(code.wrapping_sub(BTN_LEFT) as usize) {
            return send(&[mouse(if down { pressed } else { released }, 0, 0, which)]);
        }
        let Some((scan, extended)) = scan_code(code) else { return };
        if let Some(index) = SHORTCUT_MODIFIERS.iter().position(|&modifier| modifier == code) {
            let mut held = self.held.lock().unwrap();
            *held = if down { *held | 1 << index } else { *held & !(1 << index) };
        }
        let mut flags = KEYEVENTF_SCANCODE;
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if !down {
            flags |= KEYEVENTF_KEYUP;
        }
        send(&[keyboard(scan, flags)]);
    }

    /// Types text (see typing.rs): each character as itself, whatever the keyboard's layout, or as a US
    /// keyboard's key while Control, Alt or a Windows key makes a shortcut of it.
    pub fn text(&self, text: &str) {
        if *self.held.lock().unwrap() != 0 {
            return typing::by_keys(text, None, |code, down| self.key(code, down));
        }
        for character in typing::characters(text) {
            let mut units = [0; 2];
            for &unit in character.encode_utf16(&mut units).iter() {
                send(&[keyboard(unit, KEYEVENTF_UNICODE), keyboard(unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)]);
            }
        }
    }

    pub fn move_to(&self, x: i32, y: i32) {
        send(&[mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, x, y, 0)]);
    }

    /// Scrolls by the client's pixels, up and right where positive: a wheel's notch, 120 of Windows' units, for
    /// every 120, as on Linux.
    pub fn scroll(&self, horizontal: i32, vertical: i32) {
        let mut wheels = Vec::new();
        if vertical != 0 {
            wheels.push(mouse(MOUSEEVENTF_WHEEL, 0, 0, vertical));
        }
        if horizontal != 0 {
            wheels.push(mouse(MOUSEEVENTF_HWHEEL, 0, 0, horizontal));
        }
        send(&wheels);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_scan_codes() {
        assert_eq!(scan_code(1), Some((0x01, false)), "Escape");
        assert_eq!(scan_code(30), Some((0x1E, false)), "A");
        assert_eq!(scan_code(88), Some((0x58, false)), "F12");
        assert_eq!(scan_code(103), Some((0x48, true)), "Up");
        assert_eq!(scan_code(125), Some((0x5B, true)), "the left Windows key");
        assert_eq!(scan_code(113), None, "Mute has none here");
    }
}
