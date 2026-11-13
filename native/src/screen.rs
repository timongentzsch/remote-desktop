//! The desktop's screens: which there are, which of them a client asked for, and where the pointer has to go
//! to be at a place on it.

use crate::platform;

/// One screen and where it lies among the others, in the desktop's own units.
#[derive(Clone, Debug, PartialEq)]
pub struct Screen {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// The range pointer positions are told in, along each side: the client's (index.html) and the input device's.
pub const POINTER_RANGE: i32 = 65535;

/// The desktop's screens. None where the desktop does not say, which is treated as having the one.
pub fn list() -> Vec<Screen> {
    platform::screens().unwrap_or_default()
}

/// The screen called `wanted`, or the first where none is wanted or none is called that.
pub fn choose<'a>(screens: &'a [Screen], wanted: Option<&str>) -> Option<&'a Screen> {
    screens.iter().find(|screen| Some(screen.name.as_str()) == wanted).or(screens.first())
}

/// Where the pointer device has to point to be at (x, y) of `screen`, both in the device's range of 0..=65535.
/// The device's range covers the box around all screens, whichever is being shown.
pub fn place(screen: &Screen, all: &[Screen], x: i32, y: i32) -> (i32, i32) {
    const RANGE: i64 = POINTER_RANGE as i64;
    // One axis: the screen's start and length, the position on it, and the ends of the box.
    let along = |start: i32, length: i32, position: i32, from: i32, to: i32| {
        let on_desktop = (start - from) as i64 * RANGE + position as i64 * length as i64;
        (on_desktop / (to - from).max(1) as i64) as i32
    };
    let ends = |start: fn(&Screen) -> i32, length: fn(&Screen) -> i32| {
        let from = all.iter().map(start).min().unwrap_or(0);
        (from, all.iter().map(|screen| start(screen) + length(screen)).max().unwrap_or(1))
    };
    let ((left, right), (top, bottom)) = (ends(|screen| screen.x, |screen| screen.width), ends(|s| s.y, |s| s.height));
    (along(screen.x, screen.width, x, left, right), along(screen.y, screen.height, y, top, bottom))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(name: &str, x: i32, y: i32, width: i32, height: i32) -> Screen {
        Screen { name: name.to_owned(), x, y, width, height }
    }

    #[test]
    fn the_first_screen_stands_in_for_one_that_is_not_there() {
        let screens = [screen("left", 0, 0, 100, 100), screen("right", 100, 0, 100, 100)];
        assert_eq!(choose(&screens, Some("right")), Some(&screens[1]));
        assert_eq!(choose(&screens, Some("gone")), Some(&screens[0]));
        assert_eq!(choose(&screens, None), Some(&screens[0]));
        assert_eq!(choose(&[], Some("any")), None);
    }

    #[test]
    fn the_pointer_is_placed_on_the_screen_that_is_shown() {
        // A small screen left of the origin and a large one at it, the small one hanging lower.
        let screens = [screen("small", -1000, 500, 1000, 1000), screen("large", 0, 0, 3000, 1500)];
        let (small, large) = (&screens[0], &screens[1]);
        assert_eq!(place(large, &screens, 0, 0), (65535 / 4, 0), "the large screen starts a quarter along");
        assert_eq!(place(large, &screens, 65535, 65535), (65535, 65535), "and ends where the desktop does");
        assert_eq!(place(small, &screens, 0, 0), (0, 65535 / 3), "the small one starts a third down");
        assert_eq!(place(small, &screens, 65535, 65535), (65535 / 4, 65535), "and ends a quarter along");
        let alone = [screen("only", 0, 0, 1920, 1080)];
        assert_eq!(place(&alone[0], &alone, 12345, 54321), (12345, 54321), "one screen is the whole desktop");
    }
}
