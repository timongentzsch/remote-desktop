//! Screen capture. Desktops hand out the screen's picture in different ways; the one that applies feeds a `Stream`.

mod kwin;
mod mutter;
mod pipewire;
mod wayland;

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
    /// The bitrate cap in kbit/s the client now wants; 0 until it asks for a change.
    pub kbps: AtomicU32,
    /// How long converting and encoding the latest frame took, in microseconds.
    pub encode_micros: AtomicU32,
}

/// Receives each piece of the stream; returns false once nobody is listening any more.
pub type Sink = Box<dyn FnMut(&[u8]) -> bool + Send>;

/// Captures the first output until `shared.stop` is set, the sink declines a piece, or the desktop ends it.
pub fn run(settings: Settings, shared: Arc<Shared>, sink: Sink) -> Result<(), String> {
    let stream = Stream::new(settings, shared, sink);
    let offered = compositor::protocols()?;
    let offers = |protocol: &str| offered.iter().any(|name| name == protocol);
    if offers(wayland::PROTOCOL) {
        wayland::run(stream)
    } else if offers(kwin::PROTOCOL) {
        kwin::run(stream)
    } else {
        // GNOME's compositor has no Wayland protocol for this; it is asked over D-Bus.
        mutter::run(stream)
    }
}
