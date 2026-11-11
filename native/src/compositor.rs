//! What every use of the Wayland compositor here starts with: the connection, the list of what the compositor
//! offers, and one wording for what can go wrong.

use std::fmt::Display;
use std::os::fd::{AsRawFd, RawFd};
use std::time::Duration;

use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};

/// Whether the session has a Wayland compositor. If not, it may be an X11 session.
pub fn available() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("WAYLAND_SOCKET").is_some()
}

/// Connects to the session's compositor and asks what it offers.
pub fn connect<State>() -> Result<(Connection, GlobalList, EventQueue<State>), String>
where
    State: Dispatch<WlRegistry, GlobalListContents> + 'static,
{
    let connection = Connection::connect_to_env().map_err(|error| format!("no Wayland display: {error}"))?;
    let (globals, events) = registry_queue_init(&connection).map_err(|error| error.to_string())?;
    Ok((connection, globals, events))
}

/// Binds the object through which one of the compositor's protocols is used.
pub fn bind<Interface, State>(globals: &GlobalList, queue: &QueueHandle<State>) -> Result<Interface, String>
where
    Interface: Proxy + 'static,
    State: Dispatch<Interface, ()> + 'static,
{
    globals.bind(queue, 1..=1, ()).map_err(|error| format!("the compositor lacks a protocol this needs: {error}"))
}

pub fn lost(error: impl Display) -> String {
    format!("lost the Wayland connection: {error}")
}

/// One turn of talking to the compositor: sends what is pending, waits up to `limit` for it to say something
/// or for `other` to become readable, and lets `state` handle what it said.
pub fn turn<State>(
    events: &mut EventQueue<State>,
    state: &mut State,
    other: Option<RawFd>,
    limit: Duration,
) -> Result<(), String> {
    events.flush().map_err(lost)?;
    if let Some(guard) = events.prepare_read() {
        let wait = |fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let mut polled = [wait(guard.connection_fd().as_raw_fd()), wait(other.unwrap_or(-1))]; // a negative one is skipped
        unsafe { libc::poll(polled.as_mut_ptr(), 2, limit.as_millis() as i32) };
        if polled[0].revents != 0 {
            guard.read().map_err(lost)?;
        }
    }
    events.dispatch_pending(state).map_err(lost)?;
    Ok(())
}

/// The names of the protocols the compositor offers.
pub fn protocols() -> Result<Vec<String>, String> {
    struct Asking;
    impl Dispatch<WlRegistry, GlobalListContents> for Asking {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    let (_, globals, _) = connect::<Asking>()?;
    Ok(globals.contents().with_list(|list| list.iter().map(|global| global.interface.clone()).collect()))
}
