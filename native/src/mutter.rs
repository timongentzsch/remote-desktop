//! Screen capture on GNOME. Mutter, GNOME's compositor, is asked over D-Bus to cast the primary monitor and
//! delivers the pictures as a PipeWire stream. The cast lasts as long as the D-Bus connection that asked for it.

use std::time::Duration;

use rustbus::connection::Timeout;
use rustbus::message_builder::MarshalledMessage;
use rustbus::params::{Container, Param};
use rustbus::wire::ObjectPath;
use rustbus::{MessageBuilder, MessageType, RpcConn, standard_messages};

use crate::pipewire::Screen;
use crate::stream::Stream;

const SERVICE: &str = "org.gnome.Mutter.ScreenCast";
const SESSION: &str = "org.gnome.Mutter.ScreenCast.Session";
const WAIT: Timeout = Timeout::Duration(Duration::from_secs(5));
/// Mutter's cursor modes: left out of the picture, or painted into it.
const CURSOR_HIDDEN: u32 = 0;
const CURSOR_EMBEDDED: u32 = 1;

/// Captures the primary monitor until the stream is no longer wanted or GNOME ends the cast.
pub fn run(mut stream: Stream) -> Result<(), String> {
    let unavailable = "this desktop offers no screen capture this program knows: it needs the ext-image-copy-capture-v1 protocol or GNOME";
    let mut bus = RpcConn::session_conn(WAIT).map_err(|_| unavailable)?;
    bus.set_filter(Box::new(|message| matches!(message.typ, MessageType::Reply | MessageType::Error | MessageType::Signal)));
    let mut listen = standard_messages::add_match(&format!("type='signal',sender='{SERVICE}'"));
    send(&mut bus, &mut listen)?;

    let nothing = || dictionary(Vec::new());
    let session = send(&mut bus, &mut call("/org/gnome/Mutter/ScreenCast", SERVICE, "CreateSession", &[nothing()])?).map_err(|_| unavailable)?;
    let session = path(&session)?;
    let cursor = if stream.settings.cursor { CURSOR_EMBEDDED } else { CURSOR_HIDDEN };
    let options = dictionary(vec![("cursor-mode", Container::make_variant(cursor).into())]);
    // An empty connector name stands for the primary monitor.
    send(&mut bus, &mut call(&session, SESSION, "RecordMonitor", &["".into(), options])?)?;
    send(&mut bus, &mut call(&session, SESSION, "Start", &[])?)?;

    // Mutter announces the stream's PipeWire node once the session has started.
    let node = loop {
        let signal = bus.wait_signal(WAIT).map_err(|error| format!("GNOME did not start the screen cast: {error}"))?;
        if signal.dynheader.member.as_deref() == Some("PipeWireStreamAdded") {
            break signal.body.parser().get::<u32>().map_err(|error| format!("GNOME sent an unreadable stream announcement: {error:?}"))?;
        }
    };
    let mut screen = Screen::open(node, stream.settings.fps).ok_or("could not receive the screen from PipeWire")?;
    while stream.wanted() {
        match screen.next(stream.patience()) {
            Ok(Some(picture)) => stream.picture(picture.pixels, picture.stride, picture.width, picture.height),
            Ok(None) => {}
            Err(()) => return Err("GNOME ended the screen cast".into()),
        }
        stream.refine();
    }
    stream.ended.take().unwrap_or(Ok(()))
}

/// A method call to Mutter's screen-cast service.
fn call(object: &str, interface: &str, method: &str, arguments: &[Param]) -> Result<MarshalledMessage, String> {
    let mut message = MessageBuilder::new().call(method).on(object).with_interface(interface).at(SERVICE).build();
    message.body.push_old_params(arguments).map_err(|error| format!("cannot build the {method} request: {error:?}"))?;
    Ok(message)
}

/// Sends a request and waits for its answer.
fn send(bus: &mut RpcConn, message: &mut MarshalledMessage) -> Result<MarshalledMessage, String> {
    let failed = |error: rustbus::connection::Error| format!("lost the connection to the desktop's message bus: {error}");
    let serial = bus.send_message(message).map_err(failed)?.write_all().map_err(|(_, error)| failed(error))?;
    let reply = bus.wait_response(serial, WAIT).map_err(failed)?;
    match reply.typ {
        MessageType::Error => Err(format!("GNOME refused a screen cast request: {}", reply.body.parser().get::<&str>().unwrap_or("no reason given"))),
        _ => Ok(reply),
    }
}

/// The object path a reply carries.
fn path(reply: &MarshalledMessage) -> Result<String, String> {
    let path = reply.body.parser().get::<ObjectPath<&str>>().map_err(|error| format!("GNOME sent an unreadable reply: {error:?}"))?;
    Ok(path.as_ref().to_owned())
}

/// A D-Bus dictionary of named options (`a{sv}`).
fn dictionary(entries: Vec<(&'static str, Param<'static, 'static>)>) -> Param<'static, 'static> {
    Container::make_dict("s", "v", entries.into_iter()).expect("options are strings mapped to variants").into()
}
