//! The clipboard of a Wayland session, through whichever data-control protocol the compositor offers.

mod ext;
mod wlr;

use super::{Copied, Inbox};
use crate::capture::Shared;
use crate::platform::linux::compositor;

/// Whether a compositor that offers these protocols has one for the clipboard.
pub fn offered(protocols: &[String]) -> bool {
    protocols.iter().any(|name| name == ext::PROTOCOL || name == wlr::PROTOCOL)
}

pub fn run(inbox: &Inbox, shared: &Shared, copied: Copied) -> Result<(), String> {
    // The standard protocol where there is one.
    if compositor::protocols()?.iter().any(|name| name == ext::PROTOCOL) {
        ext::run(inbox, shared, copied)
    } else {
        wlr::run(inbox, shared, copied)
    }
}
