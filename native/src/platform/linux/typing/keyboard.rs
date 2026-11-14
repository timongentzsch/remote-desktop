//! A keyboard of the host's own beside the desktop's, by the virtual-keyboard protocol of wlroots, whose layout
//! is the characters to type.

use std::fs::File;
use std::io::Write;
use std::os::fd::{AsFd, FromRawFd};
use std::time::Instant;

use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_keyboard::{KeyState, KeymapFormat};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

use super::{FIRST_CODE, SETTLE, learn};
use crate::platform::linux::compositor;
use crate::typing::characters;

/// The keys a layout here gives characters to, by evdev's codes: those that type on any keyboard, the rows of
/// digits and letters and the space bar. A desktop takes a key for a shortcut by where it is on its own
/// keyboard, whatever this layout says it makes: the key that is Print there took a screenshot on Hyprland.
/// Nobody makes a shortcut of a letter alone.
const KEYS: [u16; MOST_KEYS] = {
    let (mut keys, mut at, mut code) = ([0; MOST_KEYS], 0, 2);
    while code <= 57 {
        if matches!(code, 2..=13 | 16..=27 | 30..=41 | 43..=53 | 57) {
            keys[at] = code;
            at += 1;
        }
        code += 1;
    }
    keys
};
const MOST_KEYS: usize = 48;
/// The keyboard layout, in XKB's text form, whose key `KEYS[at]` makes `characters[at]`.
fn layout(characters: &[char]) -> String {
    let codes = (0..characters.len()).map(|at| format!("<K{at}> = {};", FIRST_CODE + KEYS[at]));
    let symbols =
        characters.iter().enumerate().map(|(at, &character)| format!("key <K{at}> {{[U{:04X}]}};", character as u32));
    format!(
        "xkb_keymap {{\n\
         xkb_keycodes \"typed\" {{ minimum = {FIRST_CODE}; maximum = 255; {} }};\n\
         xkb_types \"typed\" {{ include \"complete\" }};\n\
         xkb_compatibility \"typed\" {{ include \"complete\" }};\n\
         xkb_symbols \"typed\" {{ {} }};\n\
         }};\n",
        codes.collect::<String>(),
        symbols.collect::<String>(),
    )
}

/// Nothing the compositor says matters here.
struct Deaf;

impl Dispatch<WlRegistry, GlobalListContents> for Deaf {
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
delegate_noop!(Deaf: ignore WlSeat);
delegate_noop!(Deaf: ZwpVirtualKeyboardManagerV1);
delegate_noop!(Deaf: ZwpVirtualKeyboardV1);

/// A keyboard of the host's own beside the desktop's, with a layout of the characters it has typed so far.
/// Tried on Hyprland 0.57 and on Sway, with a terminal and an Electron program that are Wayland clients; the
/// Electron program took everything but characters beyond the first 65536, an emoji.
pub struct Keyboard {
    events: EventQueue<Deaf>,
    seat: WlSeat,
    manager: ZwpVirtualKeyboardManagerV1,
    keyboard: ZwpVirtualKeyboardV1,
    known: Vec<char>,
    since: Instant,
}

impl Keyboard {
    pub fn open() -> Result<Self, String> {
        let (_, globals, events) = compositor::connect::<Deaf>()?;
        let queue = events.handle();
        let seat: WlSeat = compositor::bind(&globals, &queue)?;
        let manager: ZwpVirtualKeyboardManagerV1 = compositor::bind(&globals, &queue)?;
        let keyboard = manager.create_virtual_keyboard(&seat, &queue, ());
        // It begins with the commonest characters, so that ordinary typing seldom takes a new layout.
        let known = ('a'..='z').chain(" .,".chars()).collect();
        let mut opened = Self { events, seat, manager, keyboard, known, since: Instant::now() };
        opened.tell_layout()?;
        Ok(opened)
    }

    pub fn write(&mut self, text: &str) -> Result<(), String> {
        let text: Vec<char> = characters(text).collect();
        for piece in text.chunks(MOST_KEYS) {
            if learn(&mut self.known, piece, MOST_KEYS) {
                self.tell_layout()?;
                // The compositor has taken the layout before the first key of it is pressed.
                self.events.roundtrip(&mut Deaf).map_err(compositor::lost)?;
                std::thread::sleep(SETTLE); // Sway drops what a new keyboard types at once
            }
            for character in piece {
                let Some(key) = self.known.iter().position(|known| known == character) else { continue };
                for state in [KeyState::Pressed, KeyState::Released] {
                    self.keyboard.key(self.since.elapsed().as_millis() as u32, KEYS[key].into(), state.into());
                }
            }
        }
        // What the compositor makes of it, a refusal included, is heard before the next text.
        self.events.roundtrip(&mut Deaf).map(|_| ()).map_err(compositor::lost)
    }

    /// Hands the compositor the layout of the characters known, as a file in memory. It goes with a keyboard
    /// of its own: Hyprland tells the window that is typed into the first layout a keyboard has, and no later
    /// one.
    fn tell_layout(&mut self) -> Result<(), String> {
        self.keyboard.destroy();
        self.keyboard = self.manager.create_virtual_keyboard(&self.seat, &self.events.handle(), ());
        let layout = layout(&self.known);
        let made = unsafe { libc::memfd_create(c"layout".as_ptr(), libc::MFD_CLOEXEC) };
        if made < 0 {
            return Err(format!("no file in memory for the layout: {}", std::io::Error::last_os_error()));
        }
        let mut file = unsafe { File::from_raw_fd(made) };
        file.write_all(layout.as_bytes()).and_then(|()| file.write_all(&[0])).map_err(|error| error.to_string())?;
        self.keyboard.keymap(KeymapFormat::XkbV1.into(), file.as_fd(), layout.len() as u32 + 1);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layout_has_a_key_for_each_character() {
        let layout = layout(&['a', 'ä', '🙂']);
        assert!(layout.contains("<K0> = 10;<K1> = 11;<K2> = 12;"), "the keys of 1, 2 and 3");
        assert_eq!((KEYS[0], KEYS[12], KEYS[47]), (2, 16, 57), "digits, letters, and the space bar last");
        assert!(layout.contains("key <K0> {[U0061]};key <K1> {[U00E4]};key <K2> {[U1F642]};"));
    }

    /// The layout as the compositor will read it: libxkbcommon, where this machine has it, takes it and finds
    /// each character on its key.
    #[test]
    fn the_layout_is_one_xkb_reads() {
        use std::ffi::{CString, c_char, c_int, c_void};
        type Compile = unsafe extern "C" fn(*mut c_void, *const c_char, c_int, c_int) -> *mut c_void;
        type Symbols = unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut *const u32) -> c_int;
        let library = unsafe { libc::dlopen(c"libxkbcommon.so.0".as_ptr(), libc::RTLD_NOW) };
        if library.is_null() {
            return;
        }
        let find = |name: &std::ffi::CStr| unsafe { libc::dlsym(library, name.as_ptr()) };
        let context: unsafe extern "C" fn(c_int) -> *mut c_void =
            unsafe { std::mem::transmute(find(c"xkb_context_new")) };
        let compile: Compile = unsafe { std::mem::transmute(find(c"xkb_keymap_new_from_string")) };
        let symbols: Symbols = unsafe { std::mem::transmute(find(c"xkb_keymap_key_get_syms_by_level")) };
        let characters = ['a', 'Z', ' ', '?', 'ä', '€', '🙂'];
        let text = CString::new(layout(&characters)).unwrap();
        let keymap = unsafe { compile(context(0), text.as_ptr(), 1, 0) };
        assert!(!keymap.is_null(), "libxkbcommon refuses the layout");
        for (at, character) in characters.into_iter().enumerate() {
            let mut made = std::ptr::null();
            let count = unsafe { symbols(keymap, (FIRST_CODE + KEYS[at]).into(), 0, 0, &mut made) };
            let expected = if (character as u32) < 0x100 { character as u32 } else { 0x0100_0000 | character as u32 };
            assert_eq!((count, unsafe { *made }), (1, expected), "the key for {character:?}");
        }
    }
}
