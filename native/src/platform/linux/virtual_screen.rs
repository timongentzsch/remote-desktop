//! A screen of the size of the client's window, for a client that asked to match its device. Hyprland makes a
//! headless output for it and switches the machine's own screens off meanwhile (headless.rs); an X server
//! gives its screen a mode of that size (mode.rs). The machine's screens are as they were when the client
//! goes. Elsewhere the session shows one of the machine's screens as usual.

mod headless;
mod mode;

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::{compositor, hyprland};
use crate::screen::Window;

/// How long a session that ends may take to give the screen back to the next, as when the client reconnects
/// with other settings.
const HANDOVER: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);
/// One client at a time has the machine's screens changed for it.
static IN_USE: AtomicBool = AtomicBool::new(false);

/// The screen while it is the client's. Dropping it gives the machine its own back.
pub struct VirtualScreen {
    made: Made,
    /// Let go once `made` has put things back: fields go in this order.
    _turn: Turn,
}

enum Made {
    Headless(headless::Output),
    Mode(Box<mode::Fitted>),
}

/// This host's one client with a screen of its own.
struct Turn;

impl Turn {
    fn take() -> Result<Self, String> {
        let taken = || IN_USE.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok();
        if wait_for(HANDOVER, taken) { Ok(Self) } else { Err("another client has the screen made for it".into()) }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        IN_USE.store(false, Ordering::SeqCst);
    }
}

impl VirtualScreen {
    pub fn name(&self) -> &str {
        match &self.made {
            Made::Headless(_) => headless::NAME,
            Made::Mode(fitted) => fitted.name(),
        }
    }

    /// The screen is made at the client's own scale: there is none the desktop chose to tell.
    pub fn scale_percent(&self) -> Option<u32> {
        None
    }

    /// Makes the screen for `window` at `fps` frames a second.
    pub fn create(window: Window, fps: u32, _screen: Option<&str>) -> Result<Self, String> {
        let headless = hyprland::running();
        if !headless && compositor::available() {
            return Err("this desktop cannot make a screen of the client's size".into());
        }
        let turn = Turn::take()?;
        let made = if headless {
            Made::Headless(headless::Output::create(window, fps)?)
        } else {
            Made::Mode(Box::new(mode::Fitted::create(window, fps)?))
        };
        Ok(Self { made, _turn: turn })
    }

    /// Gives the screen the size of the client's window as it is now.
    pub fn resize(&mut self, window: Window) -> Result<(), String> {
        match &mut self.made {
            Made::Headless(output) => output.resize(window),
            Made::Mode(fitted) => fitted.resize(window),
        }
    }
}

/// Makes the screen that is up as it was made, in a desktop that has forgotten how (see hyprland.rs).
pub fn set_again() {
    headless::set_again();
}

/// Gives the machine its screens back if a host ended while a client had one made, as one that crashed would.
pub fn restore_leftover() {
    headless::restore_leftover();
    if !compositor::available() {
        mode::restore_leftover();
    }
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
