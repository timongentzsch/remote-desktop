//! Text that a client typed on a keyboard that says what it wrote, not which keys: a phone's. The host makes
//! the characters appear, where the desktop has a way to (see the platform's `Input::text`). Where it has
//! none, and while Ctrl, Alt or Super is held, each character is instead the key that makes it: a shortcut is
//! a key, and a keyboard that types text for itself knows nothing of the keys another holds. Which key that
//! is, the desktop's own keyboard layout says where it is known, and a US keyboard otherwise.

use std::collections::HashMap;

/// evdev's codes of Shift and of the keys that make a shortcut of what is typed with them: Ctrl, Alt and Super,
/// left and right.
pub const SHIFT: u16 = 42;
pub const ALT_GR: u16 = 100;
pub const MODIFIERS: [u16; 6] = [29, 97, 56, 100, 125, 126];
/// The most characters taken from one message.
pub const MOST: usize = 500;

/// A US keyboard's rows: the code of the first key, what the keys make, and what they make with Shift.
const ROWS: [(u16, &str, &str); 5] = [
    (2, "1234567890-=", "!@#$%^&*()_+"),
    (16, "qwertyuiop[]", "QWERTYUIOP{}"),
    (30, "asdfghjkl;'`", "ASDFGHJKL:\"~"),
    (43, "\\zxcvbnm,./", "|ZXCVBNM<>?"),
    (57, " ", " "),
];

/// A keyboard layout, turned around: for a character the key that makes it, and on which of the key's levels,
/// which is 1 with Shift held, 2 with AltGr and 3 with both.
pub type Layout = HashMap<char, (u16, u8)>;

/// The key that makes `character` on a US keyboard, and whether with Shift.
pub fn key_for(character: char) -> Option<(u16, bool)> {
    ROWS.iter().find_map(|&(first, plain, shifted)| {
        let key = |row: &str| row.chars().position(|made| made == character).map(|at| first + at as u16);
        key(plain).map(|key| (key, false)).or_else(|| key(shifted).map(|key| (key, true)))
    })
}

/// What of a message is typed: its first characters, without those that are no characters to see.
pub fn characters(text: &str) -> impl Iterator<Item = char> {
    text.chars().filter(|character| !character.is_control()).take(MOST)
}

/// Types `text` as keys through `key`, which presses or releases one: the keys of `layout`, and a US
/// keyboard's for what that does not have or where there is none. What neither has is left out.
pub fn by_keys(text: &str, layout: Option<&Layout>, key: impl Fn(u16, bool)) {
    for character in characters(text) {
        let known = layout.and_then(|layout| layout.get(&character).copied());
        let Some((code, level)) = known.or_else(|| key_for(character).map(|(code, shift)| (code, shift.into()))) else {
            continue;
        };
        let held = [(SHIFT, level & 1 != 0), (ALT_GR, level & 2 != 0)];
        for (modifier, _) in held.iter().filter(|(_, held)| *held) {
            key(*modifier, true);
        }
        key(code, true);
        key(code, false);
        for (modifier, _) in held.iter().filter(|(_, held)| *held) {
            key(*modifier, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_us_keyboard_makes_its_characters_and_no_others() {
        assert_eq!(key_for('a'), Some((30, false)));
        assert_eq!(key_for('A'), Some((30, true)));
        assert_eq!(key_for('1'), Some((2, false)));
        assert_eq!(key_for('?'), Some((53, true)));
        assert_eq!(key_for('\\'), Some((43, false)));
        assert_eq!(key_for('z'), Some((44, false)));
        assert_eq!(key_for(' '), Some((57, false)));
        assert_eq!(key_for('ä'), None);
        let pressed = std::cell::RefCell::new(Vec::new());
        by_keys("aB\nä", None, |code, down| pressed.borrow_mut().push((code, down)));
        let expected = [(30, true), (30, false), (SHIFT, true), (48, true), (48, false), (SHIFT, false)];
        assert_eq!(*pressed.borrow(), expected);
        // A German keyboard has z where a US one has y, and @ on q's key with AltGr.
        let german = Layout::from([('z', (21, 0)), ('@', (16, 2))]);
        pressed.borrow_mut().clear();
        by_keys("z@a", Some(&german), |code, down| pressed.borrow_mut().push((code, down)));
        let expected = [
            (21, true),
            (21, false),
            (ALT_GR, true),
            (16, true),
            (16, false),
            (ALT_GR, false),
            (30, true),
            (30, false),
        ];
        assert_eq!(*pressed.borrow(), expected, "its own keys, and a US keyboard's for the rest");
    }
}
