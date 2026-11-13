//! Screen capture on Linux. Desktops hand out the screen's picture in different ways. Each way is a module here
//! with one function, `run`, which feeds pictures to a `Stream` for as long as the stream wants them.

mod kwin;
mod memory;
mod mutter;
mod pipewire;
mod wayland;
mod x11;

use std::sync::Arc;

use super::compositor;
use crate::capture::{Settings, Shared, Sink};
use crate::stream::Stream;

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
