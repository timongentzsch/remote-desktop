//! The data-control protocol of wlroots compositors, which the standard one was made from.

use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_device_v1::{
    self as device, ZwlrDataControlDeviceV1 as Device,
};
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_manager_v1::ZwlrDataControlManagerV1 as Manager;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_offer_v1::{
    self as offer, ZwlrDataControlOfferV1 as Offer,
};
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_source_v1::{
    self as source, ZwlrDataControlSourceV1 as Source,
};

/// The protocol's name, by which a compositor announces it.
pub const PROTOCOL: &str = "zwlr_data_control_manager_v1";

// One file for both protocols, on purpose: see its first lines.
#[allow(clippy::duplicate_mod)]
#[path = "data_control.rs"]
mod data_control;

pub use data_control::run;
