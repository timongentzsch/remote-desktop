//! The desktop's own keyboard layout, turned around: for each character the key that makes it. The compositor
//! tells every client that asks for the seat's keyboard its layout, in XKB's text form.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::fs::FileExt;

use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_keyboard::{self, KeymapFormat, WlKeyboard};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};

use super::FIRST_CODE;
use crate::platform::linux::compositor;
use crate::typing::Layout;

/// What a compositor says of its seat's keyboard: that there is one, and its layout as a file.
#[derive(Default)]
struct Hearing {
    has_keyboard: bool,
    layout: Option<(OwnedFd, u32)>,
}

impl Dispatch<WlRegistry, GlobalListContents> for Hearing {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for Hearing {
    fn event(state: &mut Self, _: &WlSeat, event: wl_seat::Event, (): &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(has) } = event {
            state.has_keyboard = has.contains(wl_seat::Capability::Keyboard);
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Hearing {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap { format: WEnum::Value(KeymapFormat::XkbV1), fd, size } = event {
            state.layout = Some((fd, size));
        }
    }
}

/// The layout of the desktop's keyboard, as the compositor tells every client that asks for the keyboard.
pub fn of_desktop() -> Option<Layout> {
    let (_, globals, mut events) = compositor::connect::<Hearing>().ok()?;
    let seat: WlSeat = compositor::bind(&globals, &events.handle()).ok()?;
    let mut hearing = Hearing::default();
    events.roundtrip(&mut hearing).ok()?;
    if !hearing.has_keyboard {
        return None; // asking for a keyboard that is not there is an error
    }
    seat.get_keyboard(&events.handle(), ());
    events.roundtrip(&mut hearing).ok()?;
    let (file, size) = hearing.layout?;
    let mut text = vec![0; size as usize];
    File::from(file).read_exact_at(&mut text, 0).ok()?;
    turned_around(CStr::from_bytes_until_nul(&text).ok()?)
}

/// Reads a layout in XKB's text form and turns it around, with the library every Wayland desktop has for
/// that, libxkbcommon. Of a key's levels the first four count, which by XKB's usual types are the plain one,
/// the one with Shift, with AltGr, and with both. A character goes to the lowest level that makes it.
fn turned_around(text: &CStr) -> Option<Layout> {
    type Made = unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut *const u32) -> c_int;
    let library = unsafe { libc::dlopen(c"libxkbcommon.so.0".as_ptr(), libc::RTLD_NOW) };
    let find = |name: &CStr| {
        (!library.is_null()).then(|| unsafe { libc::dlsym(library, name.as_ptr()) }).filter(|found| !found.is_null())
    };
    // As xkbcommon.h declares them.
    let context: unsafe extern "C" fn(c_int) -> *mut c_void = unsafe { std::mem::transmute(find(c"xkb_context_new")?) };
    let compile: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, c_int) -> *mut c_void =
        unsafe { std::mem::transmute(find(c"xkb_keymap_new_from_string")?) };
    let bound: unsafe extern "C" fn(*mut c_void) -> u32 =
        unsafe { std::mem::transmute(find(c"xkb_keymap_max_keycode")?) };
    let levels: unsafe extern "C" fn(*mut c_void, u32, u32) -> u32 =
        unsafe { std::mem::transmute(find(c"xkb_keymap_num_levels_for_key")?) };
    let made: Made = unsafe { std::mem::transmute(find(c"xkb_keymap_key_get_syms_by_level")?) };
    let character: unsafe extern "C" fn(u32) -> u32 = unsafe { std::mem::transmute(find(c"xkb_keysym_to_utf32")?) };
    const XKB_TEXT_V1: c_int = 1;
    let keymap = unsafe { compile(context(0), text.as_ptr(), XKB_TEXT_V1, 0) };
    if keymap.is_null() {
        return None;
    }
    let mut layout = Layout::new();
    for level in 0..4 {
        // The keys the host's own keyboard has (see input.rs), by XKB's codes.
        for code in u32::from(FIRST_CODE) + 1..=unsafe { bound(keymap) }.min(u32::from(FIRST_CODE) + 247) {
            let mut symbols = std::ptr::null();
            if level < unsafe { levels(keymap, code, 0) } && unsafe { made(keymap, code, 0, level, &mut symbols) } == 1
            {
                let typed = char::from_u32(unsafe { character(*symbols) }).filter(|typed| !typed.is_control());
                if let Some(typed) = typed {
                    layout.entry(typed).or_insert(((code - u32::from(FIRST_CODE)) as u16, level as u8));
                }
            }
        }
    }
    Some(layout)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A German keyboard, as libxkbcommon makes it where this machine has it and the layouts' descriptions:
    /// z and y have changed places, ä has a key, and @ and € need AltGr.
    #[test]
    fn a_german_layout_is_turned_around() {
        #[repr(C)]
        struct Names([*const c_char; 5]);
        type Named = unsafe extern "C" fn(*mut c_void, *const Names, c_int) -> *mut c_void;
        let library = unsafe { libc::dlopen(c"libxkbcommon.so.0".as_ptr(), libc::RTLD_NOW) };
        if library.is_null() {
            return;
        }
        let find = |name: &CStr| unsafe { libc::dlsym(library, name.as_ptr()) };
        let context: unsafe extern "C" fn(c_int) -> *mut c_void =
            unsafe { std::mem::transmute(find(c"xkb_context_new")) };
        let named: Named = unsafe { std::mem::transmute(find(c"xkb_keymap_new_from_names")) };
        let written: unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_char =
            unsafe { std::mem::transmute(find(c"xkb_keymap_get_as_string")) };
        let null = std::ptr::null();
        let german = Names([null, c"pc105".as_ptr(), c"de".as_ptr(), null, null]);
        let keymap = unsafe { named(context(0), &german, 0) };
        if keymap.is_null() {
            return;
        }
        let layout = turned_around(unsafe { CStr::from_ptr(written(keymap, 1)) }).expect("a layout");
        let key = |character| layout.get(&character).copied();
        assert_eq!((key('z'), key('y')), (Some((21, 0)), Some((44, 0))));
        assert_eq!((key('a'), key('A'), key('ä')), (Some((30, 0)), Some((30, 1)), Some((40, 0))));
        assert_eq!(
            (key('@'), key('€'), key('1'), key('!')),
            (Some((16, 2)), Some((18, 2)), Some((2, 0)), Some((2, 1)))
        );
    }
}
