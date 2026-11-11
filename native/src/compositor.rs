//! What every use of the Wayland compositor here starts with: the connection, the list of what the compositor
//! offers, and one wording for what can go wrong.

use std::fmt::Display;

use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};

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

/// The names of the protocols the compositor offers.
pub fn protocols() -> Result<Vec<String>, String> {
    struct Asking;
    impl Dispatch<WlRegistry, GlobalListContents> for Asking {
        fn event(_: &mut Self, _: &WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
    }
    let (_, globals, _) = connect::<Asking>()?;
    Ok(globals.contents().with_list(|list| list.iter().map(|global| global.interface.clone()).collect()))
}
