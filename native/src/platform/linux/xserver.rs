//! What the uses of the X server here share: waiting for it without missing what it has already sent.

use std::os::fd::{AsRawFd, RawFd};
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::errors::ConnectionError;
use x11rb::protocol::Event;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use crate::screen::Screen;

/// The events of one connection, in the order the server sent them.
#[derive(Default)]
pub struct Events {
    /// An event `wait` had to read to find out that there was one.
    ahead: Option<Event>,
}

impl Events {
    /// The next event the server has sent, without waiting for one.
    pub fn next(&mut self, x: &RustConnection) -> Result<Option<Event>, ConnectionError> {
        match self.ahead.take() {
            Some(event) => Ok(Some(event)),
            None => x.poll_for_event(),
        }
    }

    /// Sends what is pending, then waits until the server sends something, `other` becomes readable or `limit`
    /// has passed. It returns at once if an event is in hand already: answers to requests come over the same
    /// socket, and reading one also reads the events before it, after which the socket has nothing to wake for.
    pub fn wait(&mut self, x: &RustConnection, other: Option<RawFd>, limit: Duration) -> Result<(), ConnectionError> {
        x.flush()?;
        self.ahead = x.poll_for_event()?;
        if self.ahead.is_none() {
            crate::platform::unix::fd::wait_readable([x.stream().as_raw_fd(), other.unwrap_or(-1)], Some(limit));
        }
        Ok(())
    }
}

/// The monitors the X server's one large screen is made of, in its own order.
pub fn screens() -> Result<Vec<Screen>, String> {
    let list = || -> Result<Vec<Screen>, Box<dyn std::error::Error>> {
        let (x, screen) = x11rb::connect(None)?;
        x.randr_query_version(1, 5)?.reply()?;
        let monitors = x.randr_get_monitors(x.setup().roots[screen].root, true)?.reply()?.monitors;
        let mut screens = Vec::new();
        for monitor in monitors {
            let name = String::from_utf8_lossy(&x.get_atom_name(monitor.name)?.reply()?.name).into_owned();
            let (x, y, width, height) =
                (monitor.x.into(), monitor.y.into(), monitor.width.into(), monitor.height.into());
            screens.push(Screen { name, x, y, width, height });
        }
        Ok(screens)
    };
    list().map_err(|error| format!("the X server does not list its screens: {error}"))
}
