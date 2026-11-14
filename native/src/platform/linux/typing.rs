//! Typing text into the desktop, whatever its keyboard layout (see typing.rs). A compositor built on wlroots,
//! and Hyprland, lets a client be a keyboard with a layout of its own: one whose keys are just the characters
//! to type. An X server lets a spare key be given any character. GNOME's and KDE's compositors offer neither.

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
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{ConnectionExt as _, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, Window};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::compositor;
use crate::typing::characters;

/// What types text into the desktop.
pub enum Typist {
    Wayland(Keyboard),
    X(Box<SpareKey>),
}

impl Typist {
    pub fn open() -> Result<Self, String> {
        if compositor::available() {
            Keyboard::open().map(Self::Wayland)
        } else {
            SpareKey::open().map(|key| Self::X(Box::new(key)))
        }
    }

    pub fn write(&mut self, text: &str) -> Result<(), String> {
        match self {
            Self::Wayland(keyboard) => keyboard.write(text),
            Self::X(key) => key.write(text).map_err(|error| format!("the X server takes no text: {error}")),
        }
    }
}

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
/// A layout's key codes are evdev's and this, by an old rule of X's.
const FIRST_CODE: u16 = 8;

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

/// Adds the characters of `piece`, which are no more than a layout has keys for, to those `known`, forgetting
/// the old ones where there is not room for all. Says whether there are new ones among them.
fn learn(known: &mut Vec<char>, piece: &[char]) -> bool {
    let before = known.len();
    let add = |known: &mut Vec<char>| {
        piece.iter().for_each(|character| {
            if !known.contains(character) {
                known.push(*character);
            }
        })
    };
    add(known);
    if known.len() > MOST_KEYS {
        known.clear();
        add(known);
        return true;
    }
    known.len() > before
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
/// Tried on Hyprland 0.57 with a terminal that is a Wayland client.
pub struct Keyboard {
    events: EventQueue<Deaf>,
    seat: WlSeat,
    manager: ZwpVirtualKeyboardManagerV1,
    keyboard: ZwpVirtualKeyboardV1,
    known: Vec<char>,
    since: Instant,
}

impl Keyboard {
    fn open() -> Result<Self, String> {
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

    fn write(&mut self, text: &str) -> Result<(), String> {
        let text: Vec<char> = characters(text).collect();
        for piece in text.chunks(MOST_KEYS) {
            if learn(&mut self.known, piece) {
                self.tell_layout()?;
                // The compositor has taken the layout before the first key of it is pressed.
                self.events.roundtrip(&mut Deaf).map_err(compositor::lost)?;
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

/// A key of the X server's keyboard that makes nothing, which is given each character in turn.
pub struct SpareKey {
    x: RustConnection,
    root: Window,
    key: u8,
}

type Failed = Box<dyn std::error::Error>;

impl SpareKey {
    fn open() -> Result<Self, String> {
        let open = || -> Result<Self, Failed> {
            let (x, screen) = x11rb::connect(None)?;
            x.xtest_get_version(2, 2)?.reply()?;
            let (first, last, root) = (x.setup().min_keycode, x.setup().max_keycode, x.setup().roots[screen].root);
            let keys = x.get_keyboard_mapping(first, last - first + 1)?.reply()?;
            let spare = keys
                .keysyms
                .chunks(keys.keysyms_per_keycode.into())
                .rposition(|made| made.iter().all(|&symbol| symbol == 0));
            let key = first + spare.ok_or("its keyboard has no key to spare")? as u8;
            Ok(Self { x, root, key })
        };
        open().map_err(|error| format!("the X server takes no text: {error}"))
    }

    fn write(&mut self, text: &str) -> Result<(), Failed> {
        // A character's symbol is its number, from Latin-1 on with a mark that says so.
        let symbols = characters(text).map(|character| match character as u32 {
            latin @ ..0x100 => latin,
            other => 0x0100_0000 | other,
        });
        for symbol in symbols.chain([0]) {
            self.x.change_keyboard_mapping(1, self.key, 1, &[symbol])?;
            if symbol != 0 {
                self.x.sync()?; // the new meaning is known before the key is pressed
                for event in [KEY_PRESS_EVENT, KEY_RELEASE_EVENT] {
                    self.x.xtest_fake_input(event, self.key, 0, self.root, 0, 0, 0)?;
                }
            }
            self.x.sync()?;
        }
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
        let mut known = vec!['a'];
        assert!(!learn(&mut known, &['a', 'a']), "nothing new");
        assert!(learn(&mut known, &['b', 'a', 'b']));
        assert_eq!(known, ['a', 'b']);
        let many: Vec<char> = ('c'..).take(MOST_KEYS).collect();
        assert!(learn(&mut known, &many));
        assert_eq!(known, many, "the old ones made room");
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
