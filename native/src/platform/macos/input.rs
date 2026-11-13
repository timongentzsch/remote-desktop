//! Keys and the pointer, posted as CGEvents. Keys come as Linux's evdev codes, which are turned into macOS's
//! key codes here; the pointer comes in the range of the box around all screens (see screen.rs).

use std::ffi::c_void;
use std::io;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::screen::POINTER_RANGE;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: f64,
    y: f64,
}

type Event = *mut c_void;

unsafe extern "C" {
    /// In capture.m.
    fn ask_permissions();
    fn permitted() -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceCreate(state: i32) -> *mut c_void;
    fn CGEventCreateKeyboardEvent(source: *mut c_void, key: u16, down: bool) -> Event;
    fn CGEventCreateMouseEvent(source: *mut c_void, kind: u32, at: Point, button: u32) -> Event;
    fn CGEventCreateScrollWheelEvent2(source: *mut c_void, units: u32, wheels: u32, y: i32, x: i32, z: i32) -> Event;
    fn CGEventSetIntegerValueField(event: Event, field: u32, value: i64);
    fn CGEventSetFlags(event: Event, flags: u64);
    fn CGEventSetType(event: Event, kind: u32);
    fn CGEventPost(tap: u32, event: Event);
    fn CFRelease(object: *mut c_void);
}

const HID_TAP: u32 = 0;
/// Events as from the keyboard and mouse, so that the keys they hold count everywhere, the login window too.
const HID_SYSTEM_STATE: i32 = 1;
const FLAGS_CHANGED: u32 = 12;
const MOUSE_MOVED: u32 = 5;
const SCROLL_IN_PIXELS: u32 = 0;
/// What the user is told while the host waits for its permissions, and how often it looks whether it has them.
pub const WAITING: &str = "waiting to be allowed under System Settings, Privacy & Security, in both Screen & \
System Audio Recording and Accessibility";
const PERMISSION_CHECK: Duration = Duration::from_secs(1);
/// The field of a mouse event that says which click of a double or triple click it is.
const CLICK_STATE: u32 = 1;
/// Presses of the same button closer together than this are one double (or triple) click: macOS's default.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);
/// Each button's events: (down, up, dragged), as CGEventType numbers them for the left, right and other buttons.
const BUTTON_EVENTS: [(u32, u32, u32); 3] = [(1, 2, 6), (3, 4, 7), (25, 26, 27)];
/// The evdev codes of the pointer's buttons, left, right, middle and on, as index.html sends them.
const BTN_LEFT: u16 = 0x110;
const BUTTONS: u16 = 5;

/// Modifier keys, as macOS key codes, and the flag each holds down.
const MODIFIERS: [(u16, u64); 8] = [
    (56, 0x20000),  // shift
    (60, 0x20000),  // right shift
    (59, 0x40000),  // control
    (62, 0x40000),  // right control
    (58, 0x80000),  // option
    (61, 0x80000),  // right option
    (55, 0x100000), // command
    (54, 0x100000), // right command
];

/// evdev's key codes and macOS's for the same key.
#[rustfmt::skip]
const KEYS: &[(u16, u16)] = &[
    (1, 53), (2, 18), (3, 19), (4, 20), (5, 21), (6, 23), (7, 22), (8, 26), (9, 28), (10, 25), (11, 29),
    (12, 27), (13, 24), (14, 51), (15, 48), (16, 12), (17, 13), (18, 14), (19, 15), (20, 17), (21, 16),
    (22, 32), (23, 34), (24, 31), (25, 35), (26, 33), (27, 30), (28, 36), (29, 59), (30, 0), (31, 1),
    (32, 2), (33, 3), (34, 5), (35, 4), (36, 38), (37, 40), (38, 37), (39, 41), (40, 39), (41, 50),
    (42, 56), (43, 42), (44, 6), (45, 7), (46, 8), (47, 9), (48, 11), (49, 45), (50, 46), (51, 43),
    (52, 47), (53, 44), (54, 60), (55, 67), (56, 58), (57, 49), (58, 57), (59, 122), (60, 120), (61, 99),
    (62, 118), (63, 96), (64, 97), (65, 98), (66, 100), (67, 101), (68, 109), (69, 71), (71, 89), (72, 91),
    (73, 92), (74, 78), (75, 86), (76, 87), (77, 88), (78, 69), (79, 83), (80, 84), (81, 85), (82, 82),
    (83, 65), (86, 10), (87, 103), (88, 111), (96, 76), (97, 62), (98, 75), (100, 61), (102, 115),
    (103, 126), (104, 116), (105, 123), (106, 124), (107, 119), (108, 125), (109, 121), (110, 114),
    (111, 117), (113, 74), (114, 73), (115, 72), (117, 81), (125, 55), (126, 54),
];

#[derive(Default)]
struct Held {
    /// The modifier flags and the buttons held down, and where the pointer is, in points.
    flags: u64,
    buttons: u16,
    at: Point,
    /// The last press, of which button, and which click it was.
    pressed: Option<(Instant, u16)>,
    clicks: i64,
}

pub struct Input {
    source: *mut c_void,
    held: Mutex<Held>,
}

// The event source is only read by the calls that make events, which may come from any thread.
unsafe impl Send for Input {}
unsafe impl Sync for Input {}

/// Posts `event` with the modifiers held, and lets it go.
fn post(event: Event, flags: u64) {
    if event.is_null() {
        return;
    }
    unsafe {
        CGEventSetFlags(event, flags);
        CGEventPost(HID_TAP, event);
        CFRelease(event);
    }
}

impl Input {
    /// Waits until the user has let the host record the screen and control the Mac, which macOS asks them.
    pub fn open() -> io::Result<Self> {
        if !unsafe { permitted() } {
            unsafe { ask_permissions() };
            eprintln!("{}: {WAITING}", crate::NAME);
            while !unsafe { permitted() } {
                std::thread::sleep(PERMISSION_CHECK);
            }
        }
        Ok(Self { source: unsafe { CGEventSourceCreate(HID_SYSTEM_STATE) }, held: Mutex::default() })
    }

    pub fn key(&self, code: u16, down: bool) {
        let mut held = self.held.lock().unwrap();
        if (BTN_LEFT..BTN_LEFT + BUTTONS).contains(&code) {
            let button = code - BTN_LEFT;
            let (pressed, released, _) = BUTTON_EVENTS[(button as usize).min(2)];
            held.buttons = if down { held.buttons | 1 << button } else { held.buttons & !(1 << button) };
            if down {
                let again = held.pressed.is_some_and(|(at, last)| last == button && at.elapsed() < DOUBLE_CLICK);
                held.clicks = if again { held.clicks + 1 } else { 1 };
                held.pressed = Some((Instant::now(), button));
            }
            let kind = if down { pressed } else { released };
            let event = unsafe { CGEventCreateMouseEvent(self.source, kind, held.at, button as u32) };
            unsafe { CGEventSetIntegerValueField(event, CLICK_STATE, held.clicks) };
            post(event, held.flags);
            return;
        }
        let Some(&(_, key)) = KEYS.iter().find(|(evdev, _)| *evdev == code) else { return };
        let event = unsafe { CGEventCreateKeyboardEvent(self.source, key, down) };
        if let Some(&(_, flag)) = MODIFIERS.iter().find(|(modifier, _)| *modifier == key) {
            held.flags = if down { held.flags | flag } else { held.flags & !flag };
            unsafe { CGEventSetType(event, FLAGS_CHANGED) };
        }
        post(event, held.flags);
    }

    pub fn move_to(&self, x: i32, y: i32) {
        let screens = super::screens().unwrap_or_default();
        let ends = |start: fn(&crate::screen::Screen) -> i32, length: fn(&crate::screen::Screen) -> i32| {
            let from = screens.iter().map(start).min().unwrap_or(0);
            (from, screens.iter().map(|screen| start(screen) + length(screen)).max().unwrap_or(from))
        };
        let ((left, right), (top, bottom)) = (ends(|s| s.x, |s| s.width), ends(|s| s.y, |s| s.height));
        let along = |from: i32, to: i32, at: i32| from as f64 + (to - from) as f64 * at as f64 / POINTER_RANGE as f64;
        let mut held = self.held.lock().unwrap();
        held.at = Point { x: along(left, right, x), y: along(top, bottom, y) };
        // A move while a button is held is a drag of the lowest one held.
        let kind = match held.buttons.trailing_zeros() as usize {
            button if button < BUTTONS as usize => BUTTON_EVENTS[button.min(2)].2,
            _ => MOUSE_MOVED,
        };
        let button = held.buttons.trailing_zeros().min(BUTTONS as u32 - 1);
        post(unsafe { CGEventCreateMouseEvent(self.source, kind, held.at, button) }, held.flags);
    }

    /// Scrolls by the client's pixels, up and right where positive.
    pub fn scroll(&self, horizontal: i32, vertical: i32) {
        let held = self.held.lock().unwrap();
        let event =
            unsafe { CGEventCreateScrollWheelEvent2(self.source, SCROLL_IN_PIXELS, 2, vertical, -horizontal, 0) };
        post(event, held.flags);
    }
}
