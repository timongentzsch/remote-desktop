//! The clipboard of an X11 session. X has no clipboard as such: an application says it owns the `CLIPBOARD`
//! selection and hands its content to whoever asks. This asks whenever the owner changes, and becomes the
//! owner itself to offer text from the client.

use std::sync::atomic::Ordering;

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, SELECTION_NOTIFY_EVENT,
    SelectionNotifyEvent, SelectionRequestEvent, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::{Copied, Inbox, LIMIT};
use crate::capture::Shared;
use crate::xserver::Events;

pub fn run(inbox: &Inbox, shared: &Shared, copied: Copied) -> Result<(), String> {
    share(inbox, shared, copied).map_err(|error| format!("the X server: {error}"))
}

fn share(inbox: &Inbox, shared: &Shared, mut copied: Copied) -> Result<(), Box<dyn std::error::Error>> {
    let (x, screen) = x11rb::connect(None)?;
    let root = x.setup().roots[screen].root;
    let atom = |name: &str| -> Result<Atom, Box<dyn std::error::Error>> {
        Ok(x.intern_atom(false, name.as_bytes())?.reply()?.atom)
    };
    let (clipboard, text, targets, received) =
        (atom("CLIPBOARD")?, atom("UTF8_STRING")?, atom("TARGETS")?, atom("REMOTE_DESKTOP")?);

    // Selections are held and asked for in a window's name; this one is never shown.
    let window = x.generate_id()?;
    x.create_window(0, window, root, 0, 0, 1, 1, 0, WindowClass::INPUT_ONLY, 0, &CreateWindowAux::new())?.check()?;
    x.xfixes_query_version(4, 0)?.reply()?;
    x.xfixes_select_selection_input(window, clipboard, SelectionEventMask::SET_SELECTION_OWNER)?.check()?;
    // What is on the clipboard already goes to the client too.
    x.convert_selection(window, clipboard, text, received, x11rb::CURRENT_TIME)?;

    // The text this side offers while it owns the selection, and the one last passed in either direction, so
    // that it is not passed straight back.
    let (mut own, mut last) = (String::new(), String::new());
    let mut events = Events::default();
    while !shared.stop.load(Ordering::Relaxed) {
        while let Some(event) = events.next(&x)? {
            match event {
                // Someone copied something: ask for it as text, unless it was this side.
                Event::XfixesSelectionNotify(change) if change.owner != window => {
                    x.convert_selection(window, clipboard, text, received, change.timestamp)?;
                }
                // The answer: the text is in a property of the window, or the owner has none to give.
                Event::SelectionNotify(answer) if answer.property == received => {
                    let property = x.get_property(true, window, received, text, 0, (LIMIT / 4) as u32)?.reply()?;
                    let new = String::from_utf8_lossy(&property.value).into_owned();
                    // Longer texts, and ones the owner would send in instalments, are left alone.
                    if property.type_ == text && property.bytes_after == 0 && new != last {
                        last.clone_from(&new);
                        copied(new);
                    }
                }
                // An application on the desktop is pasting: hand it the text, or the list of forms it comes in.
                Event::SelectionRequest(request) => answer(&x, &request, [targets, text], &own)?,
                _ => {}
            }
        }
        if let Some(new) = inbox.take().filter(|new| *new != last) {
            (own, last) = (new.clone(), new);
            x.set_selection_owner(window, clipboard, x11rb::CURRENT_TIME)?;
        }
        events.wait(&x, Some(inbox.waker()), super::LOOK)?;
    }
    Ok(())
}

/// Answers an application that asks for this side's selection: with the text, with the list of forms the text
/// comes in, or with a refusal for any other form.
fn answer(
    x: &RustConnection,
    request: &SelectionRequestEvent,
    [targets, text]: [Atom; 2],
    own: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Applications from before 1988 name no property; they mean the one called like the target.
    let property = if request.property == x11rb::NONE { request.target } else { request.property };
    let given = if request.target == text {
        x.change_property8(PropMode::REPLACE, request.requestor, property, text, own.as_bytes())?;
        property
    } else if request.target == targets {
        x.change_property32(PropMode::REPLACE, request.requestor, property, AtomEnum::ATOM, &[targets, text])?;
        property
    } else {
        x11rb::NONE
    };
    let reply = SelectionNotifyEvent {
        response_type: SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: request.time,
        requestor: request.requestor,
        selection: request.selection,
        target: request.target,
        property: given,
    };
    x.send_event(false, request.requestor, EventMask::NO_EVENT, reply)?;
    Ok(())
}
