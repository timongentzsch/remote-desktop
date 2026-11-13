//! A screen made to the size of the client's window, for a client that asked to match its device: a headless
//! output of the compositor, which the session shows instead of the machine's own screens. Those are switched
//! off meanwhile, so that windows move over to it and nobody at the machine watches, and come back on when it
//! goes. Only Hyprland makes one so far; elsewhere the session shows one of the machine's screens as usual.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::hyprland;
use crate::screen::{self, Window};

/// The output's name, which the capture finds it by.
pub const NAME: &str = crate::NAME;
/// How long the compositor may take to announce the new output to the capture,
const APPEARING: Duration = Duration::from_secs(2);
/// and a session that ends may take to give the screen back to the next, as when the client reconnects with
/// other settings.
const HANDOVER: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);
/// One virtual screen at a time: it takes over the machine's screens for one client.
static IN_USE: AtomicBool = AtomicBool::new(false);

/// The virtual screen while it is there. Dropping it brings the machine's own screens back.
pub struct VirtualScreen {
    fps: u32,
}

impl VirtualScreen {
    /// Makes the screen for `window` at `fps` frames a second and switches the machine's own screens off.
    pub fn create(window: Window, fps: u32) -> Result<Self, String> {
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
        create_output()?;
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
            switch_off(&own.name)?;
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
        Ok(made)
    }

    /// Gives the screen the size of the client's window as it is now.
    pub fn resize(&self, window: Window) -> Result<(), String> {
        set_mode(window.pixels(), self.fps, window.scale())
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
    if let Err(error) = restore_outputs() {
        warn!("could not bring the screens back: {error}");
    }
    let _ = std::fs::remove_file(marker());
}

/// Notes that a virtual screen is up.
fn marker() -> PathBuf {
    hyprland::marker("remote-desktop-screen")
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

/// The headless output on Hyprland, the only desktop that makes one so far.
fn create_output() -> Result<(), String> {
    hyprland::hyprctl(&["output", "create", "headless", NAME])
}

fn set_mode((width, height): (u32, u32), fps: u32, scale: f64) -> Result<(), String> {
    let mode = format!("{width}x{height}@{fps}");
    hyprland::monitor(
        &format!(r#"{{ output = "{NAME}", mode = "{mode}", position = "auto", scale = {scale} }}"#),
        &format!("{NAME},{mode},auto,{scale}"),
    )
}

fn switch_off(name: &str) -> Result<(), String> {
    hyprland::monitor(&format!(r#"{{ output = "{name}", disabled = true }}"#), &format!("{name},disable"))
}

/// Reading the configuration again brings its screens back as they were set up; then the headless one goes.
fn restore_outputs() -> Result<(), String> {
    let reloaded = hyprland::hyprctl(&["reload"]);
    hyprland::hyprctl(&["output", "remove", NAME]).and(reloaded)
}
