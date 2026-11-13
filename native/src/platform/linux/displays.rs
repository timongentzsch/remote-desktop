//! Displays that went to sleep: capture gets no frames from one, and a key press wakes it.

use std::fs;
use std::thread;
use std::time::Duration;

use super::input::{Input, KEY_LEFTSHIFT};

/// The kernel's displays, whose power state says whether one sleeps, and how long one may take to wake and how
/// often that is looked at.
const DISPLAYS: &str = "/sys/class/drm";
const WAKING: Duration = Duration::from_secs(3);
const WAKE_LOOK: Duration = Duration::from_millis(100);

/// Capture gets no frames from a sleeping display, and a key press wakes it.
pub fn wake_displays(input: &Input) {
    let asleep = || {
        fs::read_dir(DISPLAYS).into_iter().flatten().flatten().any(|connector| {
            let read = |name: &str| fs::read_to_string(connector.path().join(name)).unwrap_or_default();
            read("status") == "connected\n" && read("dpms") != "On\n"
        })
    };
    if asleep() {
        input.key(KEY_LEFTSHIFT, true);
        input.key(KEY_LEFTSHIFT, false);
        for _ in 0..WAKING.as_millis() / WAKE_LOOK.as_millis() {
            if !asleep() {
                break;
            }
            thread::sleep(WAKE_LOOK);
        }
    }
}
