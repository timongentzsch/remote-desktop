//! The clipboard of a Wayland session, through whichever data-control protocol the compositor offers.

mod ext;
mod wlr;

use super::{Copied, Inbox};
use crate::capture::Shared;
use crate::compositor;

pub fn run(inbox: &Inbox, shared: &Shared, copied: Copied) -> Result<(), String> {
    let offered = compositor::protocols()?;
    let offers = |protocol: &str| offered.iter().any(|name| name == protocol);
    if offers(ext::PROTOCOL) {
        ext::run(inbox, shared, copied)
    } else if offers(wlr::PROTOCOL) {
        wlr::run(inbox, shared, copied)
    } else {
        Err("the compositor offers no way to share the clipboard".into())
    }
}
