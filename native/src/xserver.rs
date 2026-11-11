//! What the uses of the X server here share: waiting for it without missing what it has already sent.

use std::os::fd::{AsRawFd, RawFd};
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::errors::ConnectionError;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

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
            let wait = |fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            let mut polled = [wait(x.stream().as_raw_fd()), wait(other.unwrap_or(-1))]; // a negative one is skipped
            unsafe { libc::poll(polled.as_mut_ptr(), 2, limit.as_millis() as i32) };
        }
        Ok(())
    }
}
