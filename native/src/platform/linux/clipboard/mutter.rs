//! The clipboard of a GNOME session. GNOME offers programs no Wayland protocol for it; Mutter's remote-desktop
//! service takes its place: it says when the clipboard changes and hands out its content, and takes content
//! to offer in turn.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::os::fd::FromRawFd;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rustbus::message_builder::MarshalledMessage;
use rustbus::params::Container;
use rustbus::wire::UnixFd;
use rustbus::wire::unmarshal::traits::Variant;

use super::{Copied, Inbox, TEXT_TYPES, read_text};
use crate::capture::Shared;
use crate::platform::linux::gnome::{self, Service, unreadable};

pub const SERVICE: &str = "org.gnome.Mutter.RemoteDesktop";
const SESSION: &str = "org.gnome.Mutter.RemoteDesktop.Session";

pub fn run(inbox: &Inbox, shared: &Shared, mut copied: Copied) -> Result<(), String> {
    let mut mutter = Service::connect(SERVICE)?;
    let session = gnome::path(&mutter.call("/org/gnome/Mutter/RemoteDesktop", SERVICE, "CreateSession", &[])?)?;
    mutter.call(&session, SESSION, "Start", &[])?;
    mutter.call(&session, SESSION, "EnableClipboard", &[gnome::options(Vec::new())])?;

    // The text this side offers, and the one last passed in either direction, so that it is not passed
    // straight back.
    let (mut own, mut last) = (String::new(), String::new());
    while !shared.stop.load(Ordering::Relaxed) {
        if let Some(signal) = mutter.signal(Duration::from_millis(100))? {
            match signal.dynheader.member.as_deref() {
                // Something was copied on the desktop: fetch it if it is text and not this side's own.
                Some("SelectionOwnerChanged") => {
                    let change = signal.body.parser().get::<HashMap<String, Variant>>().map_err(unreadable)?;
                    let ours = change.get("session-is-owner").and_then(|ours| ours.get::<bool>().ok()).unwrap_or(false);
                    let kinds =
                        change.get("mime-types").and_then(|kinds| kinds.get::<Vec<String>>().ok()).unwrap_or_default();
                    let kind = TEXT_TYPES.iter().find(|kind| kinds.iter().any(|offered| offered == *kind));
                    // By the time it is asked for, the content may have been replaced; then Mutter refuses.
                    let content = match (ours, kind) {
                        (false, Some(kind)) => mutter.call(&session, SESSION, "SelectionRead", &[(*kind).into()]).ok(),
                        _ => None,
                    };
                    let text = content.and_then(|reply| file(&reply).ok()).and_then(read_text);
                    if let Some(text) = text.filter(|text| *text != last) {
                        last.clone_from(&text);
                        copied(text);
                    }
                }
                // An application on the desktop is pasting: write the text where Mutter says.
                Some("SelectionTransfer") => {
                    let mut request = signal.body.parser();
                    let (_kind, serial) =
                        (request.get::<&str>().map_err(unreadable)?, request.get::<u32>().map_err(unreadable)?);
                    // Mutter refuses if the application has stopped waiting meanwhile, which is no harm.
                    if let Ok(reply) = mutter.call(&session, SESSION, "SelectionWrite", &[serial.into()]) {
                        let written = file(&reply)?.write_all(own.as_bytes()).is_ok();
                        let _ = mutter.call(&session, SESSION, "SelectionWriteDone", &[serial.into(), written.into()]);
                    }
                }
                Some("Closed") => return Err("GNOME closed the session".into()),
                _ => {}
            }
        }
        if let Some(new) = inbox.take().filter(|new| *new != last) {
            (own, last) = (new.clone(), new);
            let kinds = Container::make_array("s", TEXT_TYPES.into_iter()).expect("an array of strings");
            mutter.call(&session, SESSION, "SetSelection", &[gnome::options(vec![("mime-types", kinds.into())])])?;
        }
    }
    Ok(())
}

/// The file a reply carries, through which clipboard content is read or written.
fn file(reply: &MarshalledMessage) -> Result<File, String> {
    let descriptor = reply.body.parser().get::<UnixFd>().map_err(unreadable)?;
    let descriptor = descriptor.take_raw_fd().ok_or("GNOME sent no file for the clipboard's content")?;
    Ok(unsafe { File::from_raw_fd(descriptor) })
}
