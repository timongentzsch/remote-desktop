//! Typing text into the desktop, whatever its keyboard layout (see typing.rs). Each way of doing it is a module
//! here, and `Typist` chooses among them by what the desktop offers, never by which desktop it is:
//!
//! - `keyboard`: a compositor that lets a client be a keyboard (Hyprland, Sway and others built on wlroots)
//!   is given one whose layout is just the characters to type.
//! - `spare_key`: an X server lets its keyboard's spare keys be given any character.
//! - `layout`: the desktop's own keyboard layout, turned around, for what has to be typed as keys after all:
//!   shortcuts, a compositor that offers neither of the above (GNOME's and KDE's), and the windows of X
//!   programs under a compositor.
//!
//! What a desktop turned out to need beyond its protocol's letter is done for all of them, and said where it
//! is done, with the desktop it was seen on.

mod keyboard;
mod layout;
mod spare_key;

use std::time::Duration;

use keyboard::Keyboard;
use spare_key::SpareKey;

use super::compositor;
use crate::typing::Layout;

/// A desktop is given this long to take in a keyboard or a key's meaning that is new to it, before it is typed
/// with.
const SETTLE: Duration = Duration::from_millis(30);
/// A layout's key codes are evdev's and this, by an old rule of X's.
const FIRST_CODE: u16 = 8;

type Failed = Box<dyn std::error::Error>;

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
            let layout = layout::of_desktop();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn characters_are_learned_until_there_is_no_room() {
        const MOST: usize = 48;
        let mut known = vec!['a'];
        assert!(!learn(&mut known, &['a', 'a'], MOST), "nothing new");
        assert!(learn(&mut known, &['b', 'a', 'b'], MOST));
        assert_eq!(known, ['a', 'b']);
        let many: Vec<char> = ('c'..).take(MOST).collect();
        assert!(learn(&mut known, &many, MOST));
        assert_eq!(known, many, "the old ones made room");
    }
}
