//! A screen made to the size of the client's window, for a client that asked to match its device: a headless
//! output of the compositor, which the session shows instead of the machine's own screens. Those are switched
//! off meanwhile, so that windows move over to it and nobody at the machine watches, and come back on when it
//! goes. Only Hyprland makes one so far; elsewhere the session shows one of the machine's screens as usual.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::screen;

/// The output's name, which the capture finds it by.
pub const NAME: &str = crate::NAME;
/// How long the compositor may take to announce the new output to the capture,
const APPEARING: Duration = Duration::from_secs(2);
/// and a session that ends may take to give the screen back to the next, as when the client reconnects with
/// other settings.
const HANDOVER: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);
/// The smallest and largest size, in pixels, made for a client.
const SMALLEST: u32 = 320;
const LARGEST: u32 = 7680;
/// The largest desktop scale, that of the densest phone screens.
const LARGEST_SCALE: f64 = 4.0;

/// One virtual screen at a time: it takes over the machine's screens for one client.
static IN_USE: AtomicBool = AtomicBool::new(false);

/// The client's window: its size in the page's units and how many pixels each of those has.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub width: f64,
    pub height: f64,
    pub pixel_ratio: f64,
}

impl Window {
    /// The screen's size in pixels: even, as 4:2:0 video needs, and within reason.
    fn pixels(&self) -> (u32, u32) {
        let side = |length: f64| ((length * self.pixel_ratio).round() as u32).clamp(SMALLEST, LARGEST) & !1;
        (side(self.width), side(self.height))
    }

    /// The screen's size in the desktop's own units, as the desktop lists it: its pixels at its scale.
    fn size(&self) -> (i32, i32) {
        let (width, height) = self.pixels();
        let unscaled = |pixels: u32| (pixels as f64 / self.scale()).round() as i32;
        (unscaled(width), unscaled(height))
    }

    /// The desktop's scale for it, so that things look as large as they would on the device itself.
    fn scale(&self) -> f64 {
        self.pixel_ratio.clamp(1.0, LARGEST_SCALE)
    }
}

/// The virtual screen while it is there. Dropping it brings the machine's own screens back.
pub struct VirtualScreen {
    fps: u32,
}

impl VirtualScreen {
    /// Makes the screen for `window` at `fps` frames a second and switches the machine's own screens off. Where
    /// the client draws the pointer itself (`pointer_drawn_by_client`), the desktop's own is hidden: a screen
    /// without a display behind it has no layer of its own for the pointer, so the desktop paints the pointer
    /// into the picture even for a capture that asked for it without, and the client would show two.
    pub fn create(window: Window, fps: u32, pointer_drawn_by_client: bool) -> Result<Self, String> {
        if !hyprland::running() {
            return Err("this desktop cannot make a screen of the client's size".into());
        }
        let taken = || IN_USE.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok();
        if !wait_for(HANDOVER, taken) {
            return Err("another client has the virtual screen".into());
        }
        // From here on, dropping it undoes what was done, also after a failure halfway.
        let made = Self { fps };
        std::fs::write(marker(), NAME).map_err(|error| format!("cannot note the virtual screen: {error}"))?;
        hyprland::create()?;
        made.resize(window)?;
        // The compositor makes the output at a size of its own and gives it the one asked for a moment later;
        // a capture that began before would start over at the new size.
        let size = window.size();
        let appeared = wait_for(APPEARING, || {
            screen::list().iter().any(|screen| screen.name == NAME && (screen.width, screen.height) == size)
        });
        if !appeared {
            return Err("the virtual screen did not appear".into());
        }
        for own in screen::list().into_iter().filter(|screen| screen.name != NAME) {
            hyprland::switch_off(&own.name)?;
        }
        // The session places the pointer by the screens the desktop lists once this returns. Until the others
        // are gone, the list still has them, and this screen beside them rather than where it ends up alone.
        let alone = wait_for(APPEARING, || {
            let screens = screen::list();
            screens.len() == 1 && screens[0].name == NAME && (screens[0].x, screens[0].y) == (0, 0)
        });
        if !alone {
            return Err("the machine's own screens did not go off".into());
        }
        if pointer_drawn_by_client {
            hyprland::hide_pointer()?;
        }
        Ok(made)
    }

    /// Gives the screen the size of the client's window as it is now.
    pub fn resize(&self, window: Window) -> Result<(), String> {
        hyprland::set_mode(window.pixels(), self.fps, window.scale())
    }
}

impl Drop for VirtualScreen {
    fn drop(&mut self) {
        restore();
        IN_USE.store(false, Ordering::SeqCst);
    }
}

/// Brings the machine's screens back if a host ended with a virtual screen up, as one that crashed would.
pub fn restore_leftover() {
    if marker().exists() && hyprland::running() {
        restore();
    }
}

fn restore() {
    if let Err(error) = hyprland::restore() {
        warn!("could not bring the screens back: {error}");
    }
    let _ = std::fs::remove_file(marker());
}

/// Notes that a virtual screen is up, where it goes with the login session.
fn marker() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from).join("remote-desktop-screen")
}

/// Whether `done` came true within `limit`, asking it every `POLL`.
fn wait_for(limit: Duration, done: impl Fn() -> bool) -> bool {
    let began = Instant::now();
    while !done() {
        if began.elapsed() > limit {
            return false;
        }
        thread::sleep(POLL);
    }
    true
}

/// Hyprland, through `hyprctl`. Its newer configuration is Lua, set with `eval`; the older one takes `keyword`.
mod hyprland {
    use super::*;

    pub fn running() -> bool {
        std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
    }

    pub fn create() -> Result<(), String> {
        hyprctl(&["output", "create", "headless", NAME])
    }

    pub fn set_mode((width, height): (u32, u32), fps: u32, scale: f64) -> Result<(), String> {
        let mode = format!("{width}x{height}@{fps}");
        monitor(
            &format!(r#"{{ output = "{NAME}", mode = "{mode}", position = "auto", scale = {scale} }}"#),
            &format!("{NAME},{mode},auto,{scale}"),
        )
    }

    pub fn switch_off(name: &str) -> Result<(), String> {
        monitor(&format!(r#"{{ output = "{name}", disabled = true }}"#), &format!("{name},disable"))
    }

    /// Nobody sits at the machine while its screens are off; `restore` shows the pointer again.
    pub fn hide_pointer() -> Result<(), String> {
        setting(r#"{ cursor = { invisible = true } }"#, &["cursor:invisible", "true"])
    }

    /// Reading the configuration again brings its screens back as they were set up, and its pointer; then the
    /// headless one goes.
    pub fn restore() -> Result<(), String> {
        let reloaded = hyprctl(&["reload"]);
        hyprctl(&["output", "remove", NAME]).and(reloaded)
    }

    /// A monitor rule: as the Lua table `lua`, or else in the older form `legacy`.
    fn monitor(lua: &str, legacy: &str) -> Result<(), String> {
        hyprctl(&["eval", &format!("hl.monitor({lua})")]).or_else(|_| hyprctl(&["keyword", "monitor", legacy]))
    }

    /// A setting: as the Lua table `lua`, or else as the older form's keyword and value, `legacy`.
    fn setting(lua: &str, legacy: &[&str; 2]) -> Result<(), String> {
        hyprctl(&["eval", &format!("hl.config({lua})")]).or_else(|_| hyprctl(&["keyword", legacy[0], legacy[1]]))
    }

    /// Runs `hyprctl`, which says "ok" when it did what it was told.
    fn hyprctl(arguments: &[&str]) -> Result<(), String> {
        let output =
            Command::new("hyprctl").args(arguments).output().map_err(|error| format!("cannot run hyprctl: {error}"))?;
        let reply = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if output.status.success() && reply == "ok" {
            Ok(())
        } else {
            Err(format!("hyprctl {}: {reply}", arguments.join(" ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
