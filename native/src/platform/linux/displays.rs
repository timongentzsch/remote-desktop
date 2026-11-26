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

/// Whether the displays sleep, by the kernel's word for each connector, its status and its power state: some
/// are connected and none of those is on. One that is off beside one that is on, as a laptop's own under a
/// closed lid, is out of use, which no key press changes.
fn asleep(connectors: impl Iterator<Item = (String, String)>) -> bool {
    let mut connected = connectors.filter(|(status, _)| status == "connected\n").peekable();
    connected.peek().is_some() && connected.all(|(_, power)| power != "On\n")
}

/// Capture gets no frames from a sleeping display, and a key press wakes it.
pub fn wake_displays(input: &Input) {
    let asleep = || {
        asleep(fs::read_dir(DISPLAYS).into_iter().flatten().flatten().map(|connector| {
            let read = |name: &str| fs::read_to_string(connector.path().join(name)).unwrap_or_default();
            (read("status"), read("dpms"))
        }))
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

#[cfg(test)]
mod tests {
    #[test]
    fn displays_sleep_when_none_that_is_connected_is_on() {
        let asleep = |connectors: &[(&str, &str)]| {
            super::asleep(connectors.iter().map(|(status, power)| (format!("{status}\n"), format!("{power}\n"))))
        };
        assert!(asleep(&[("connected", "Off"), ("disconnected", "Off")]));
        assert!(asleep(&[("connected", "Off"), ("connected", "Standby")]));
        assert!(!asleep(&[("connected", "On"), ("disconnected", "Off")]));
        assert!(!asleep(&[("connected", "Off"), ("connected", "On")]), "one out of use beside one that is on");
        assert!(!asleep(&[("disconnected", "Off")]), "no display at all");
    }
}
