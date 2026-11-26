//! A headless output of the compositor at the size of the client's window, which the session shows instead of
//! the machine's own screens. Those are switched off meanwhile, so that windows move over to it and nobody at
//! the machine watches, and come back on when it goes. Only Hyprland makes one so far.

use std::path::PathBuf;
use std::time::Duration;

use super::wait_for;
use crate::platform::linux::{hyprland, pointer};
use crate::screen::{self, Window};

/// The output's name, which the capture finds it by.
pub const NAME: &str = crate::NAME;
/// How long the compositor may take to announce the new output to the capture.
const APPEARING: Duration = Duration::from_secs(2);

/// The output while it is there. Dropping it brings the machine's own screens back.
pub struct Output {
    fps: u32,
}

impl Output {
    /// Makes the output for `window` at `fps` frames a second and switches the machine's own screens off.
    pub fn create(window: Window, fps: u32) -> Result<Self, String> {
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

impl Drop for Output {
    fn drop(&mut self) {
        restore();
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
/// It also undoes what other sessions have set, which is set again.
fn restore_outputs() -> Result<(), String> {
    let reloaded = hyprland::hyprctl(&["reload"]);
    pointer::hide_again();
    hyprland::hyprctl(&["output", "remove", NAME]).and(reloaded)
}
