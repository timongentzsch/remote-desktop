//! The desktop's own pointer, hidden while a client draws its own. Hyprland 0.57 puts its pointer into every
//! capture, even one that asks to leave it out, and the client then showed the pointer twice; a screen without
//! a display behind it (see virtual_screen.rs) had it so before. Only Hyprland is asked: other desktops leave
//! the pointer out as they are asked to. Someone at the machine sees no pointer while a client is connected.

use std::sync::Mutex;

use super::hyprland;

/// How many sessions want the pointer hidden now; it shows again when the last one ends.
static HIDING: Mutex<u32> = Mutex::new(0);
const MARKER: &str = "remote-desktop-pointer";

/// The pointer stays hidden while this is kept.
pub struct HiddenPointer(());

/// Hides the desktop's pointer, where the desktop is one that would otherwise show it in the picture.
pub fn hide() -> Option<HiddenPointer> {
    if !hyprland::running() {
        return None;
    }
    let mut hiding = HIDING.lock().unwrap();
    if *hiding == 0 {
        let _ = std::fs::write(hyprland::marker(MARKER), "");
        if let Err(error) = set_invisible(true) {
            warn!("the desktop's pointer stays in the picture: {error}");
            return None;
        }
    }
    *hiding += 1;
    Some(HiddenPointer(()))
}

impl Drop for HiddenPointer {
    fn drop(&mut self) {
        let mut hiding = HIDING.lock().unwrap();
        *hiding -= 1;
        if *hiding == 0 {
            show();
        }
    }
}

/// Shows the pointer again if a host ended with it hidden, as one that crashed would.
pub fn restore_leftover() {
    if hyprland::marker(MARKER).exists() && hyprland::running() {
        show();
    }
}

fn show() {
    if let Err(error) = set_invisible(false) {
        warn!("could not show the desktop's pointer again: {error}");
    }
    let _ = std::fs::remove_file(hyprland::marker(MARKER));
}

fn set_invisible(invisible: bool) -> Result<(), String> {
    let value = if invisible { "true" } else { "false" };
    hyprland::setting(&format!("{{ cursor = {{ invisible = {value} }} }}"), ["cursor:invisible", value])
}
