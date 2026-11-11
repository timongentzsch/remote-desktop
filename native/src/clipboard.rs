//! Clipboard text in both directions, through the compositor's wlr-data-control protocol.
//!
//! One thread per session watches the desktop's clipboard and tells the client when its text changes, and puts
//! text the client copied onto the desktop's clipboard.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use wayland_client::backend::ObjectId;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop, event_created_child};
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_device_v1::{self as device, ZwlrDataControlDeviceV1};
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_manager_v1::ZwlrDataControlManagerV1;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_offer_v1::{self as offer, ZwlrDataControlOfferV1};
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_source_v1::{self as source, ZwlrDataControlSourceV1};

use crate::capture::Shared;

/// The names text goes by on the clipboard, most precise first.
const TEXT_TYPES: [&str; 4] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "STRING"];
/// Longer clipboard contents are not passed on.
const LIMIT: usize = 1 << 20;

/// Where the session leaves text the client copied, for the clipboard thread to pick up.
pub struct Inbox {
    text: Mutex<Option<String>>,
    wake: File,
}

impl Inbox {
    pub fn new() -> io::Result<Arc<Self>> {
        match unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) } {
            -1 => Err(io::Error::last_os_error()),
            fd => Ok(Arc::new(Self { text: Mutex::new(None), wake: unsafe { File::from_raw_fd(fd) } })),
        }
    }

    pub fn put(&self, text: String) {
        *self.text.lock().unwrap() = Some(text);
        let _ = (&self.wake).write_all(&1u64.to_ne_bytes());
    }
}

/// Shares the clipboard until `shared.stop` is set. `copied` is called with each new text on the desktop's clipboard.
pub fn run(inbox: Arc<Inbox>, shared: Arc<Shared>, copied: Box<dyn FnMut(String) + Send>) -> Result<(), String> {
    let connection = Connection::connect_to_env().map_err(|error| format!("no Wayland display: {error}"))?;
    let (globals, mut events) = registry_queue_init::<Clipboard>(&connection).map_err(|error| error.to_string())?;
    let queue = events.handle();
    let missing = |error| format!("the compositor lacks a protocol this needs: {error}");
    let seat: WlSeat = globals.bind(&queue, 1..=1, ()).map_err(missing)?;
    let manager: ZwlrDataControlManagerV1 = globals.bind(&queue, 1..=1, ()).map_err(missing)?;
    let device = manager.get_data_device(&seat, &queue, ());
    let mut clipboard = Clipboard { connection: connection.clone(), manager, device, offers: HashMap::new(), own: None, own_announced: false, last: String::new(), copied, finished: false };

    let lost = |error| format!("lost the Wayland connection: {error}");
    while !shared.stop.load(Ordering::Relaxed) && !clipboard.finished {
        events.flush().map_err(lost)?;
        if let Some(guard) = events.prepare_read() {
            let wait = |fd: i32| libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            let mut polled = [wait(guard.connection_fd().as_raw_fd()), wait(inbox.wake.as_raw_fd())];
            unsafe { libc::poll(polled.as_mut_ptr(), 2, 250) };
            if polled[0].revents != 0 {
                guard.read().map_err(lost)?;
            }
            if polled[1].revents != 0 {
                let _ = (&inbox.wake).read(&mut [0; 8]);
            }
        }
        if let Some(text) = inbox.text.lock().unwrap().take() {
            clipboard.set(text, &queue);
        }
        events.dispatch_pending(&mut clipboard).map_err(|error| format!("lost the Wayland connection: {error}"))?;
    }
    Ok(())
}

struct Clipboard {
    connection: Connection,
    manager: ZwlrDataControlManagerV1,
    device: ZwlrDataControlDeviceV1,
    /// The types each announced offer comes in.
    offers: HashMap<ObjectId, Vec<String>>,
    /// The selection this side put on the clipboard, and its text.
    own: Option<(ZwlrDataControlSourceV1, String)>,
    /// The compositor announces our own selection back to us; that one must not be read.
    own_announced: bool,
    /// The text last passed in either direction, so that it is not passed straight back.
    last: String,
    copied: Box<dyn FnMut(String) + Send>,
    finished: bool,
}

impl Clipboard {
    /// Puts text from the client on the desktop's clipboard.
    fn set(&mut self, text: String, queue: &QueueHandle<Self>) {
        if text == self.last {
            return;
        }
        let source = self.manager.create_data_source(queue, ());
        for kind in TEXT_TYPES {
            source.offer(kind.to_string());
        }
        self.device.set_selection(Some(&source));
        self.own_announced = true;
        self.last.clone_from(&text);
        if let Some((replaced, _)) = self.own.replace((source, text)) {
            replaced.destroy();
        }
    }

    /// Reads an offer's text. The owning application writes it into a pipe; one that stalls is given up on.
    fn receive(&self, offer: &ZwlrDataControlOfferV1, kind: &str) -> Option<String> {
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

impl Dispatch<ZwlrDataControlDeviceV1, ()> for Clipboard {
    fn event(state: &mut Self, _: &ZwlrDataControlDeviceV1, event: device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            device::Event::DataOffer { id } => {
                state.offers.insert(id.id(), Vec::new());
            }
            device::Event::Selection { id: Some(offer) } => {
                let kinds = state.offers.remove(&offer.id()).unwrap_or_default();
                let kind = TEXT_TYPES.iter().find(|kind| kinds.iter().any(|offered| offered == *kind));
                if std::mem::take(&mut state.own_announced) {
                    // our own text coming back
                } else if let Some(text) = kind.and_then(|kind| state.receive(&offer, kind)) {
                    if text != state.last {
                        state.last.clone_from(&text);
                        (state.copied)(text);
                    }
                }
                offer.destroy();
            }
            device::Event::Finished => state.finished = true,
            _ => {}
        }
    }

    event_created_child!(Clipboard, ZwlrDataControlDeviceV1, [
        device::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ZwlrDataControlOfferV1, ()> for Clipboard {
    fn event(state: &mut Self, offer: &ZwlrDataControlOfferV1, event: offer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let offer::Event::Offer { mime_type } = event {
            state.offers.entry(offer.id()).or_default().push(mime_type);
        }
    }
}

impl Dispatch<ZwlrDataControlSourceV1, ()> for Clipboard {
    fn event(state: &mut Self, source: &ZwlrDataControlSourceV1, event: source::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
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
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

delegate_noop!(Clipboard: ignore WlSeat);
delegate_noop!(Clipboard: ZwlrDataControlManagerV1);
