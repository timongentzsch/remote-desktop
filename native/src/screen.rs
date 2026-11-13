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

/// The smallest and largest size, in pixels, made for a client.
const SMALLEST: u32 = 320;
const LARGEST: u32 = 7680;
/// The largest desktop scale, that of the densest phone screens.
const LARGEST_SCALE: f64 = 4.0;

/// The client's window: its size in the page's units and how many pixels each of those has.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub width: f64,
    pub height: f64,
    pub pixel_ratio: f64,
}

impl Window {
    /// The screen's size in pixels: even, as 4:2:0 video needs, and within reason.
    pub fn pixels(&self) -> (u32, u32) {
        let side = |length: f64| ((length * self.pixel_ratio).round() as u32).clamp(SMALLEST, LARGEST) & !1;
        (side(self.width), side(self.height))
    }

    /// The screen's size in the desktop's own units, as the desktop lists it: its pixels at its scale.
    pub fn size(&self) -> (i32, i32) {
        let (width, height) = self.pixels();
        let unscaled = |pixels: u32| (pixels as f64 / self.scale()).round() as i32;
        (unscaled(width), unscaled(height))
    }

    /// The desktop's scale for it, so that things look as large as they would on the device itself.
    pub fn scale(&self) -> f64 {
        self.pixel_ratio.clamp(1.0, LARGEST_SCALE)
    }
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

    #[test]
    fn the_screen_has_the_windows_pixels_evenly_and_within_reason() {
        let window = |width, height, pixel_ratio| Window { width, height, pixel_ratio };
        assert_eq!(window(1512.0, 982.0, 2.0).pixels(), (3024, 1964));
        assert_eq!(window(1280.0, 719.5, 1.0).pixels(), (1280, 720), "rounded");
        assert_eq!(window(1001.0, 701.0, 1.0).pixels(), (1000, 700), "even");
        assert_eq!(window(100.0, 9000.0, 1.0).pixels(), (320, 7680), "within reason");
        assert_eq!(window(390.0, 844.0, 3.0).scale(), 3.0);
        assert_eq!(window(1512.0, 982.0, 2.0).size(), (1512, 982), "listed in the desktop's units");
        assert_eq!(window(800.0, 600.0, 0.5).scale(), 1.0);
    }
}
