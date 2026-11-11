//! Screen capture on GNOME. Mutter is asked to cast the primary monitor and delivers the pictures as a
//! PipeWire stream.

use std::time::Duration;

use super::pipewire;
use crate::gnome::{self, Service};
use crate::stream::Stream;

const SERVICE: &str = "org.gnome.Mutter.ScreenCast";
const SESSION: &str = "org.gnome.Mutter.ScreenCast.Session";
/// Mutter's cursor modes: left out of the picture, or painted into it.
const CURSOR_HIDDEN: u32 = 0;
const CURSOR_EMBEDDED: u32 = 1;

/// Captures the primary monitor until the stream is no longer wanted or GNOME ends the cast.
pub fn run(stream: Stream) -> Result<(), String> {
    let unavailable = "this desktop offers no screen capture this program knows: it needs a Wayland capture protocol, GNOME, KDE or X11";
    let mut mutter = Service::connect(SERVICE).map_err(|_| unavailable)?;
    let session = mutter
        .call("/org/gnome/Mutter/ScreenCast", SERVICE, "CreateSession", &[gnome::options(Vec::new())])
        .map_err(|_| unavailable)?;
    let session = gnome::path(&session)?;
    let cursor = if stream.settings.cursor { CURSOR_EMBEDDED } else { CURSOR_HIDDEN };
    // An empty connector name stands for the primary monitor.
    mutter.call(
        &session,
        SESSION,
        "RecordMonitor",
        &["".into(), gnome::options(vec![("cursor-mode", cursor.into())])],
    )?;
    mutter.call(&session, SESSION, "Start", &[])?;

    // Mutter announces the stream's PipeWire node once the session has started.
    let node = loop {
        let signal = mutter.signal(Duration::from_secs(5))?.ok_or("GNOME did not start the screen cast")?;
        if signal.dynheader.member.as_deref() == Some("PipeWireStreamAdded") {
            break signal.body.parser().get::<u32>().map_err(gnome::unreadable)?;
        }
    };
    pipewire::run(node, stream, "GNOME")
}
