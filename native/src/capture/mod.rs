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
    /// Let the stream choose `full_chroma` itself, as encoding time and the bitrate allow (see stream/mod.rs).
    pub auto_chroma: bool,
    /// The most rows the picture may have: a taller screen is shrunk to this height, keeping its shape. None
    /// leaves it at the screen's own size.
    pub height: Option<usize>,
    /// The screen to capture, by name (see screen.rs). None, or a name no screen has, means the first.
    pub screen: Option<String>,
}

/// What a session and its capture exchange while the capture runs.
#[derive(Default)]
pub struct Shared {
    /// Set by the session to end the capture.
    pub stop: AtomicBool,
    /// Set by the session when its client needs a frame it can decode without any before it.
    pub fresh: AtomicBool,
    /// The bitrate in kbit/s the session now allows; 0 until it sets one.
    pub kbps: AtomicU32,
    /// Set while that bitrate is still on its way up to what the link carries or the client wants.
    pub climbing: AtomicBool,
    /// How long converting and encoding the latest frame took, in microseconds.
    pub encode_micros: AtomicU32,
    /// How many frames the bitrate left coarser than they should be, since the session last asked.
    pub coarse: AtomicU32,
}

/// What the pointer looks like: rows of pixels, each red, green, blue and how opaque it is, and the place in
/// the picture that is at the pointer's position.
#[derive(Clone, PartialEq)]
pub struct Shape {
    pub width: u16,
    pub height: u16,
    pub hot: (u16, u16),
    pub pixels: Vec<u8>,
}

impl Shape {
    /// A shape from pixels as desktops hand them out: 32 bits each, alpha on top and blue at the bottom, the
    /// colours already multiplied by the alpha.
    pub fn from_premultiplied(width: u16, height: u16, hot: (u16, u16), argb: impl Iterator<Item = u32>) -> Self {
        let straight =
            |colour: u8, alpha: u8| (colour as u32 * 255).checked_div(alpha as u32).unwrap_or(0).min(255) as u8;
        let pixels = argb.flat_map(|pixel| {
            let [blue, green, red, alpha] = pixel.to_le_bytes();
            [straight(red, alpha), straight(green, alpha), straight(blue, alpha), alpha]
        });
        Self { width, height, hot, pixels: pixels.collect() }
    }
}

/// What a capture hands to its session.
pub enum Output<'a> {
    /// The next piece of the video stream.
    Video(&'a [u8]),
    /// The pointer's shape, where the client draws the pointer and the desktop tells its shape.
    Pointer(&'a Shape),
}

/// Receives what the capture puts out; returns false once nobody is listening any more.
pub type Sink = Box<dyn FnMut(Output) -> bool + Send>;

/// Captures the chosen screen until `shared.stop` is set, the sink declines a piece, or the desktop ends it.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shape_gets_its_colours_back_from_under_the_alpha() {
        // Opaque red, half-transparent white, fully transparent.
        let shape = Shape::from_premultiplied(3, 1, (1, 0), [0xFFFF_0000, 0x8080_8080, 0].into_iter());
        assert_eq!(shape.pixels, [255, 0, 0, 255, 255, 255, 255, 128, 0, 0, 0, 0]);
    }
}
