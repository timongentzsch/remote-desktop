//! Talking to Mutter, GNOME's compositor, over D-Bus. It has services of its own for what Wayland protocols
//! provide elsewhere, and what is asked of one lasts as long as the connection that asked.

use std::time::Duration;

use rustbus::connection::{Error, Timeout};
use rustbus::message_builder::MarshalledMessage;
use rustbus::params::{Container, Param};
use rustbus::wire::ObjectPath;
use rustbus::{MessageBuilder, MessageType, RpcConn, standard_messages};

/// How long Mutter gets to answer a call.
const PATIENCE: Duration = Duration::from_secs(5);

/// A connection to one of Mutter's services.
pub struct Service {
    bus: RpcConn,
    name: &'static str,
}

impl Service {
    /// Connects to the session's message bus and asks to be sent the service's signals.
    pub fn connect(name: &'static str) -> Result<Self, String> {
        let mut bus = RpcConn::session_conn(Timeout::Duration(PATIENCE)).map_err(lost)?;
        bus.set_filter(Box::new(|message| {
            matches!(message.typ, MessageType::Reply | MessageType::Error | MessageType::Signal)
        }));
        let mut service = Self { bus, name };
        service.send(standard_messages::add_match(&format!("type='signal',sender='{name}'")))?;
        Ok(service)
    }

    /// Calls a method of one of the service's objects and waits for the answer.
    pub fn call(
        &mut self,
        object: &str,
        interface: &str,
        method: &str,
        arguments: &[Param],
    ) -> Result<MarshalledMessage, String> {
        let mut message = MessageBuilder::new().call(method).on(object).with_interface(interface).at(self.name).build();
        message
            .body
            .push_old_params(arguments)
            .map_err(|error| format!("cannot build the {method} request: {error:?}"))?;
        self.send(message)
    }

    /// The service's next signal, waiting up to `limit` for one.
    pub fn signal(&mut self, limit: Duration) -> Result<Option<MarshalledMessage>, String> {
        match self.bus.wait_signal(Timeout::Duration(limit)) {
            Ok(signal) => Ok(Some(signal)),
            Err(Error::TimedOut) => Ok(None),
            Err(error) => Err(lost(error)),
        }
    }

    fn send(&mut self, mut message: MarshalledMessage) -> Result<MarshalledMessage, String> {
        let serial =
            self.bus.send_message(&mut message).map_err(lost)?.write_all().map_err(|(_, error)| lost(error))?;
        let reply = self.bus.wait_response(serial, Timeout::Duration(PATIENCE)).map_err(lost)?;
        match reply.typ {
            MessageType::Error => Err(format!(
                "GNOME refused a request: {}",
                reply.body.parser().get::<&str>().unwrap_or("no reason given")
            )),
            _ => Ok(reply),
        }
    }
}

fn lost(error: Error) -> String {
    format!("no connection to GNOME over the desktop's message bus: {error}")
}

/// The object path a reply carries.
pub fn path(reply: &MarshalledMessage) -> Result<String, String> {
    let path = reply.body.parser().get::<ObjectPath<&str>>().map_err(unreadable)?;
    Ok(path.as_ref().to_owned())
}

pub fn unreadable(error: impl std::fmt::Debug) -> String {
    format!("GNOME sent something unreadable: {error:?}")
}

/// A D-Bus dictionary of named options (`a{sv}`).
pub fn options(entries: Vec<(&'static str, Param<'static, 'static>)>) -> Param<'static, 'static> {
    let entries = entries.into_iter().map(|(name, value)| (name, Container::make_variant(value)));
    Container::make_dict("s", "v", entries).expect("options are strings mapped to variants").into()
}
