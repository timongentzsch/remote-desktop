//! The spare keys of an X server's keyboard, which are given the characters to type.

use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{ConnectionExt as _, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, Window};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::{FIRST_CODE, Failed, SETTLE, learn};
use crate::typing::{Layout, characters};

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

const MOST_SPARE: usize = 32;

impl SpareKey {
    pub fn open() -> Result<Self, String> {
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
    pub fn layout(&self) -> Result<Layout, Failed> {
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
    pub fn has_focus(&self) -> Result<bool, Failed> {
        const NONE_OR_ROOT: Window = 1;
        Ok(self.x.get_input_focus()?.reply()?.focus > NONE_OR_ROOT)
    }

    pub fn write(&mut self, text: &str) -> Result<(), Failed> {
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
