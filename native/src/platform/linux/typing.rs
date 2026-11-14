//! Typing text into the desktop, whatever its keyboard layout (see typing.rs). A compositor built on wlroots,
//! and Hyprland, lets a client be a keyboard with a layout of its own: one whose keys are just the characters
//! to type. An X server lets a spare key be given any character. GNOME's and KDE's compositors offer neither.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::os::unix::fs::FileExt;
use std::time::{Duration, Instant};

use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_keyboard::{self, KeyState, KeymapFormat, WlKeyboard};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{ConnectionExt as _, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, Window};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::compositor;
use crate::typing::{Layout, characters};

/// What types text into the desktop: a keyboard of the compositor's, or an X server's spare keys. A compositor
/// runs an X server for the programs that need one, and neither way reaches those: they read the keys by the
/// desktop's own layout whatever layout a keyboard of the host's brings, and what is typed through their X
/// server does not arrive (tried on Hyprland with an Electron program). So that server is asked whether a
/// window of its own is typed into, and such a window is left to the keys of the desktop's keyboard, as all
/// are on GNOME and KDE, whose compositors let no client be a keyboard.
pub struct Typist {
    keyboard: Option<Keyboard>,
    key: Option<SpareKey>,
    /// The desktop's own keyboard layout as it was when this was opened, where it could be told.
    layout: Option<Layout>,
}

impl Typist {
    pub fn open() -> Self {
        let told = |error| warn!("typed text goes by the keyboard's keys: {error}");
        if compositor::available() {
            // Its layout is asked first: a keyboard of the host's own would answer with its own.
            let layout = desktop_layout();
            Self { keyboard: Keyboard::open().map_err(told).ok(), key: SpareKey::open().ok(), layout }
        } else {
            let key = SpareKey::open().map_err(told).ok();
            let layout = key.as_ref().and_then(|key| key.layout().ok());
            Self { keyboard: None, key, layout }
        }
    }

    /// The desktop's own keyboard layout, for what is typed as keys.
    pub fn layout(&self) -> Option<&Layout> {
        self.layout.as_ref()
    }

    /// Types `text`; false where that is left to the keyboard's keys.
    pub fn write(&mut self, text: &str) -> Result<bool, String> {
        let refused = |error: Failed| format!("the X server takes no text: {error}");
        match (&mut self.keyboard, &mut self.key) {
            (Some(_), Some(key)) if key.has_focus().map_err(refused)? => return Ok(false),
            (Some(keyboard), _) => keyboard.write(text)?,
            (None, Some(key)) if !compositor::available() => key.write(text).map_err(refused)?,
            _ => return Ok(false),
        }
        Ok(true)
    }
}

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
fn desktop_layout() -> Option<Layout> {
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

/// Adds the characters of `piece`, which are no more than there are keys for, to those `known`, forgetting the
/// old ones where there is not room for all `most`. Says whether there are new ones among them.
fn learn(known: &mut Vec<char>, piece: &[char], most: usize) -> bool {
    let before = known.len();
    let add = |known: &mut Vec<char>| {
        piece.iter().for_each(|character| {
            if !known.contains(character) {
                known.push(*character);
            }
        })
    };
    add(known);
    if known.len() > most {
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

/// The keys of the X server's keyboard that make nothing, which are given the characters to type. A program
/// hears that the keys' meaning has changed and then asks the server what it is: it is given a moment for
/// that before the keys are pressed, and again before they change once more, or it would read a later meaning
/// into an earlier key. Tried with an Electron program under Hyprland's X server.
pub struct SpareKey {
    x: RustConnection,
    root: Window,
    keys: Vec<u8>,
    /// What the keys make now, in their order.
    known: Vec<char>,
}

type Failed = Box<dyn std::error::Error>;

const MOST_SPARE: usize = 32;
const SETTLE: Duration = Duration::from_millis(30);

impl SpareKey {
    fn open() -> Result<Self, String> {
        let open = || -> Result<Self, Failed> {
            let (x, screen) = x11rb::connect(None)?;
            x.xtest_get_version(2, 2)?.reply()?;
            let (first, last, root) = (x.setup().min_keycode, x.setup().max_keycode, x.setup().roots[screen].root);
            let made = x.get_keyboard_mapping(first, last - first + 1)?.reply()?;
            let spare = made.keysyms.chunks(made.keysyms_per_keycode.into()).enumerate().rev();
            let spare =
                spare.filter(|(_, made)| made.iter().all(|&symbol| symbol == 0)).map(|(at, _)| first + at as u8);
            let keys: Vec<u8> = spare.take(MOST_SPARE).collect();
            if keys.is_empty() {
                return Err("its keyboard has no key to spare".into());
            }
            Ok(Self { x, root, keys, known: Vec::new() })
        };
        open().map_err(|error| format!("the X server takes no text: {error}"))
    }

    /// The layout of the X server's keyboard, turned around. A key's symbols are told in columns: plain and
    /// with Shift, the same of a second layout, then with AltGr and with both.
    fn layout(&self) -> Result<Layout, Failed> {
        let first = self.x.setup().min_keycode;
        let made = self.x.get_keyboard_mapping(first, self.x.setup().max_keycode - first + 1)?.reply()?;
        let mut layout = Layout::new();
        for (level, column) in [0, 1, 4, 5].into_iter().enumerate() {
            for (at, made) in made.keysyms.chunks(made.keysyms_per_keycode.into()).enumerate() {
                // A symbol is its character's number: as it is in Latin-1, and marked beyond.
                let character = match made.get(column) {
                    Some(&latin @ 0x20..0x100) => char::from_u32(latin),
                    Some(&marked @ 0x0100_0000..) => char::from_u32(marked & 0xFF_FFFF),
                    _ => None,
                };
                let code = (usize::from(first) + at).saturating_sub(FIRST_CODE.into()) as u16;
                if let Some(character) = character.filter(|character| !character.is_control()) {
                    layout.entry(character).or_insert((code, level as u8));
                }
            }
        }
        Ok(layout)
    }

    /// Whether a window of this X server's is the one typed into. Under a compositor it has none while the
    /// window is the compositor's own.
    fn has_focus(&self) -> Result<bool, Failed> {
        const NONE_OR_ROOT: Window = 1;
        Ok(self.x.get_input_focus()?.reply()?.focus > NONE_OR_ROOT)
    }

    fn write(&mut self, text: &str) -> Result<(), Failed> {
        let text: Vec<char> = characters(text).collect();
        for piece in text.chunks(self.keys.len()) {
            if learn(&mut self.known, piece, self.keys.len()) {
                for (&key, &character) in self.keys.iter().zip(&self.known) {
                    // A character's symbol is its number, from Latin-1 on with a mark that says so.
                    let symbol = match character as u32 {
                        latin @ ..0x100 => latin,
                        other => 0x0100_0000 | other,
                    };
                    // Twice: a key with one symbol makes a letter's small form, and its capital only with Shift.
                    self.x.change_keyboard_mapping(1, key, 2, &[symbol, symbol])?;
                }
                self.x.sync()?;
                std::thread::sleep(SETTLE);
            }
            for character in piece {
                let Some(at) = self.known.iter().position(|known| known == character) else { continue };
                for event in [KEY_PRESS_EVENT, KEY_RELEASE_EVENT] {
                    self.x.xtest_fake_input(event, self.keys[at], 0, self.root, 0, 0, 0)?;
                }
            }
            self.x.sync()?;
            std::thread::sleep(SETTLE);
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
        assert!(!learn(&mut known, &['a', 'a'], MOST_KEYS), "nothing new");
        assert!(learn(&mut known, &['b', 'a', 'b'], MOST_KEYS));
        assert_eq!(known, ['a', 'b']);
        let many: Vec<char> = ('c'..).take(MOST_KEYS).collect();
        assert!(learn(&mut known, &many, MOST_KEYS));
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
