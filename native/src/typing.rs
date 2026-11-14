//! Text that a client typed on a keyboard that says what it wrote, not which keys: a phone's. The host makes
//! the characters appear, where the desktop has a way to (see the platform's `Input::text`). Where it has
//! none, and while Ctrl, Alt or Super is held, each character is instead the key that makes it on a US
//! keyboard: a shortcut is a key, and a keyboard that types text for itself knows nothing of the keys another
//! holds.

/// evdev's codes of Shift and of the keys that make a shortcut of what is typed with them: Ctrl, Alt and Super,
/// left and right.
pub const SHIFT: u16 = 42;
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

/// Types `text` as a US keyboard's keys through `key`, which presses or releases one. What that keyboard
/// does not have is left out.
pub fn by_keys(text: &str, key: impl Fn(u16, bool)) {
    for (code, shift) in characters(text).filter_map(key_for) {
        if shift {
            key(SHIFT, true);
        }
        key(code, true);
        key(code, false);
        if shift {
            key(SHIFT, false);
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
        by_keys("aB\nä", |code, down| pressed.borrow_mut().push((code, down)));
        let expected = [(30, true), (30, false), (SHIFT, true), (48, true), (48, false), (SHIFT, false)];
        assert_eq!(*pressed.borrow(), expected);
    }
}
