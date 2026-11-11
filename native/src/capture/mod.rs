//! Screen capture. Desktops hand out the screen's picture in different ways. Each way is a module here with one
//! function, `run`, which feeds pictures to a `Stream` for as long as the stream wants them.

mod kwin;
mod memory;
mod mutter;
mod pipewire;
mod wayland;
mod x11;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32};

use crate::compositor;
use crate::stream::Stream;

pub struct Settings {
    pub kbps: u32,
    pub fps: u32,
    /// Paint the pointer into the picture. Off when the client draws its own.
    pub cursor: bool,
    /// Keep full colour resolution (4:4:4) instead of 4:2:0.
    pub full_chroma: bool,
    pub half_size: bool,
}

/// What a session and its capture exchange while the capture runs.
#[derive(Default)]
pub struct Shared {
    /// Set by the session to end the capture.
    pub stop: AtomicBool,
    /// The bitrate in kbit/s the session now allows; 0 until it sets one.
    pub kbps: AtomicU32,
    /// Set while that bitrate is still on its way up to what the link carries or the client wants.
    pub climbing: AtomicBool,
    /// How long converting and encoding the latest frame took, in microseconds.
    pub encode_micros: AtomicU32,
}

/// Receives each piece of the stream; returns false once nobody is listening any more.
pub type Sink = Box<dyn FnMut(&[u8]) -> bool + Send>;

/// Captures the first screen until `shared.stop` is set, the sink declines a piece, or the desktop ends it.
pub fn run(settings: Settings, shared: Arc<Shared>, sink: Sink) -> Result<(), String> {
    let stream = Stream::new(settings, shared, sink);
    if !compositor::available() {
        return x11::run(stream);
    }
    let offered = compositor::protocols()?;
    let offers = |protocol: &str| offered.iter().any(|name| name == protocol);
    if wayland::offered(&offered) {
        wayland::run(stream)
    } else if offers(kwin::PROTOCOL) {
        kwin::run(stream)
    } else {
        // GNOME's compositor has no Wayland protocol for this; it is asked over D-Bus.
        mutter::run(stream)
    }
}
