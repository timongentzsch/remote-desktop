//! Keyboard and pointer injection through the kernel's uinput interface, which every desktop accepts.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const EV_ABS: u16 = 3;
const REL_HWHEEL: u16 = 6;
const REL_WHEEL: u16 = 8;
const REL_WHEEL_HI_RES: u16 = 11;
const REL_HWHEEL_HI_RES: u16 = 12;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
pub const KEY_LEFTSHIFT: u16 = 42;
/// Codes from here on are pointer buttons, not keys.
const BTN_MOUSE: u16 = 0x110;
/// The keyboard's codes run up to this one, which the kernel calls KEY_MICMUTE; the pointer has five buttons:
/// left, right, middle, back and forward.
const KEYS: u16 = 248;
const BUTTONS: u16 = 5;
/// Pointer positions arrive normalised to this range.
pub const ABS_RANGE: i32 = 65535;

// linux/uinput.h ioctls (identical on x86_64 and arm64)
const UI_DEV_CREATE: u32 = 0x5501;
const UI_DEV_SETUP: u32 = 0x405C_5503;
const UI_ABS_SETUP: u32 = 0x401C_5504;
const UI_SET_EVBIT: u32 = 0x4004_5564;
const UI_SET_KEYBIT: u32 = 0x4004_5565;
const UI_SET_RELBIT: u32 = 0x4004_5566;
const UI_SET_ABSBIT: u32 = 0x4004_5567;

/// struct uinput_setup
#[repr(C)]
struct Setup {
    id: [u16; 4], // bus, vendor, product, version
    name: [u8; 80],
    effects: u32,
}

/// struct uinput_abs_setup
#[repr(C)]
struct AbsoluteSetup {
    code: u16,
    info: [i32; 6], // value, minimum, maximum, fuzz, flat, resolution
}

pub struct Input {
    keyboard: File,
    pointer: File,
}

impl Input {
    pub fn open() -> io::Result<Self> {
        Ok(Self {
            keyboard: device("Remote desktop keyboard", &(1..KEYS).collect::<Vec<_>>(), &[], &[])?,
            pointer: device(
                "Remote desktop pointer",
                &(BTN_MOUSE..BTN_MOUSE + BUTTONS).collect::<Vec<_>>(),
                &[REL_HWHEEL, REL_WHEEL, REL_WHEEL_HI_RES, REL_HWHEEL_HI_RES],
                &[ABS_X, ABS_Y],
            )?,
        })
    }

    /// Presses or releases a key or, for codes from BTN_MOUSE up, a pointer button.
    pub fn key(&self, code: u16, down: bool) {
        emit(if code >= BTN_MOUSE { &self.pointer } else { &self.keyboard }, &[(EV_KEY, code, down as i32)]);
    }

    /// Moves the pointer to a position given in 0..=65535 on each axis.
    pub fn move_to(&self, x: i32, y: i32) {
        emit(&self.pointer, &[(EV_ABS, ABS_X, x), (EV_ABS, ABS_Y, y)]);
    }

    /// Scrolls by high-resolution wheel units, 120 per notch.
    pub fn scroll(&self, horizontal: i32, vertical: i32) {
        emit(&self.pointer, &[(EV_REL, REL_HWHEEL_HI_RES, horizontal), (EV_REL, REL_WHEEL_HI_RES, vertical)]);
    }
}

fn device(name: &str, keys: &[u16], relative: &[u16], absolute: &[u16]) -> io::Result<File> {
    let file = OpenOptions::new().write(true).open("/dev/uinput")?;
    let control = |request: u32, argument: usize| match unsafe { libc::ioctl(file.as_raw_fd(), request as _, argument) }
    {
        -1 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    };
    for (event, set_bit, codes) in
        [(EV_KEY, UI_SET_KEYBIT, keys), (EV_REL, UI_SET_RELBIT, relative), (EV_ABS, UI_SET_ABSBIT, absolute)]
    {
        if !codes.is_empty() {
            control(UI_SET_EVBIT, event as usize)?;
        }
        for &code in codes {
            control(set_bit, code as usize)?;
        }
    }
    let mut setup = Setup { id: [6, 1, 1, 1], name: [0; 80], effects: 0 }; // bus 6 = virtual
    setup.name[..name.len()].copy_from_slice(name.as_bytes());
    control(UI_DEV_SETUP, &raw const setup as usize)?;
    for &code in absolute {
        let axis = AbsoluteSetup { code, info: [0, 0, ABS_RANGE, 0, 0, 0] };
        control(UI_ABS_SETUP, &raw const axis as usize)?;
    }
    control(UI_DEV_CREATE, 0)?;
    Ok(file)
}

/// Writes (type, code, value) events followed by a sync report, as one write so they arrive together.
fn emit(mut device: &File, events: &[(u16, u16, i32)]) {
    let batch: Vec<libc::input_event> = events
        .iter()
        .chain([&(EV_SYN, 0, 0)])
        .map(|&(type_, code, value)| libc::input_event {
            time: libc::timeval { tv_sec: 0, tv_usec: 0 },
            type_,
            code,
            value,
        })
        .collect();
    let bytes = unsafe { std::slice::from_raw_parts(batch.as_ptr().cast::<u8>(), size_of_val(&batch[..])) };
    // An event the kernel refuses (an out-of-range code from the client) is simply not injected.
    let _ = device.write_all(bytes);
}
