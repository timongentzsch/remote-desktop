//! The clipboard of a Wayland session, through the compositor's ext-data-control-v1 protocol.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::Ordering;
use std::time::Duration;

use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop, event_created_child};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_device_v1::{
    self as device, ExtDataControlDeviceV1,
};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_manager_v1::ExtDataControlManagerV1;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_offer_v1::{
    self as offer, ExtDataControlOfferV1,
};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_source_v1::{
    self as source, ExtDataControlSourceV1,
};

use super::{Copied, Inbox, LIMIT};
use crate::capture::Shared;
use crate::compositor::{self, bind};

/// The names text goes by on the clipboard, most precise first.
const TEXT_TYPES: [&str; 4] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "STRING"];
/// An extra type on what this side puts on the clipboard. The compositor announces every clipboard change,
/// this side's own too; the mark tells those apart, so that they are not read back and sent to the client.
const OWN: &str = "application/x-remote-desktop-own";
pub fn run(inbox: &Inbox, shared: &Shared, copied: Copied) -> Result<(), String> {
    let (connection, globals, mut events) = compositor::connect::<Clipboard>()?;
    let queue = events.handle();
    let seat: WlSeat = bind(&globals, &queue)?;
    let manager: ExtDataControlManagerV1 = bind(&globals, &queue)?;
    let device = manager.get_data_device(&seat, &queue, ());
    let mut clipboard = Clipboard {
        connection,
        manager,
        device,
        offers: HashMap::new(),
        own: None,
        last: String::new(),
        copied,
        finished: false,
    };

    while !shared.stop.load(Ordering::Relaxed) && !clipboard.finished {
        compositor::turn(&mut events, &mut clipboard, Some(inbox.waker()), Duration::from_millis(250))?;
        if let Some(text) = inbox.take() {
            clipboard.set(text, &queue);
        }
    }
    Ok(())
}

struct Clipboard {
    connection: Connection,
    manager: ExtDataControlManagerV1,
    device: ExtDataControlDeviceV1,
    /// The types each announced offer comes in.
    offers: HashMap<ObjectId, Vec<String>>,
    /// The selection this side put on the clipboard, and its text.
    own: Option<(ExtDataControlSourceV1, String)>,
    /// The text last passed in either direction, so that it is not passed straight back.
    last: String,
    copied: Copied,
    finished: bool,
}

impl Clipboard {
    /// Puts text from the client on the desktop's clipboard.
    fn set(&mut self, text: String, queue: &QueueHandle<Self>) {
        if text == self.last {
            return;
        }
        let source = self.manager.create_data_source(queue, ());
        for kind in TEXT_TYPES.into_iter().chain([OWN]) {
            source.offer(kind.to_string());
        }
        self.device.set_selection(Some(&source));
        self.last.clone_from(&text);
        if let Some((replaced, _)) = self.own.replace((source, text)) {
            replaced.destroy();
        }
    }

    /// Reads an offer's text. The owning application writes it into a pipe; one that stalls is given up on.
    fn receive(&self, offer: &ExtDataControlOfferV1, kind: &str) -> Option<String> {
        let mut ends = [0; 2];
        if unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return None;
        }
        let (mut reading, writing) = unsafe { (File::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
        offer.receive(kind.to_string(), writing.as_fd());
        self.connection.flush().ok()?;
        drop(writing);
        let mut text = Vec::new();
        let mut chunk = [0; 65536];
        loop {
            let mut poll = libc::pollfd { fd: reading.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            if unsafe { libc::poll(&mut poll, 1, 1000) } <= 0 {
                return None;
            }
            match reading.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) if text.len() + count <= LIMIT => text.extend(&chunk[..count]),
                _ => return None,
            }
        }
        Some(String::from_utf8_lossy(&text).into_owned())
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for Clipboard {
    fn event(
        state: &mut Self,
        _: &ExtDataControlDeviceV1,
        event: device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            device::Event::DataOffer { id } => {
                state.offers.insert(id.id(), Vec::new());
            }
            device::Event::Selection { id: Some(offer) } => {
                let kinds = state.offers.remove(&offer.id()).unwrap_or_default();
                let kind = TEXT_TYPES.iter().find(|kind| kinds.iter().any(|offered| offered == *kind));
                if kinds.iter().any(|offered| offered == OWN) {
                    // our own text coming back
                } else if let Some(text) = kind.and_then(|kind| state.receive(&offer, kind))
                    && text != state.last
                {
                    state.last.clone_from(&text);
                    (state.copied)(text);
                }
                offer.destroy();
            }
            device::Event::Finished => state.finished = true,
            _ => {}
        }
    }

    event_created_child!(Clipboard, ExtDataControlDeviceV1, [
        device::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ExtDataControlOfferV1, ()> for Clipboard {
    fn event(
        state: &mut Self,
        offer: &ExtDataControlOfferV1,
        event: offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let offer::Event::Offer { mime_type } = event {
            state.offers.entry(offer.id()).or_default().push(mime_type);
        }
    }
}

impl Dispatch<ExtDataControlSourceV1, ()> for Clipboard {
    fn event(
        state: &mut Self,
        source: &ExtDataControlSourceV1,
        event: source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let current = state.own.as_ref().filter(|(own, _)| own == source);
        match event {
            // An application on the desktop is pasting: hand it the text.
            source::Event::Send { fd, .. } => {
                if let Some((_, text)) = current {
                    let _ = File::from(fd).write_all(text.as_bytes());
                }
            }
            // Something else was copied on the desktop, so our text is no longer the clipboard's.
            source::Event::Cancelled => {
                if current.is_some() {
                    state.own = None;
                }
                source.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Clipboard {
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

delegate_noop!(Clipboard: ignore WlSeat);
delegate_noop!(Clipboard: ExtDataControlManagerV1);
