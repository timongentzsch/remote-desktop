//! The standard data-control protocol, ext-data-control-v1.

use wayland_protocols::ext::data_control::v1::client::ext_data_control_device_v1::{
    self as device, ExtDataControlDeviceV1 as Device,
};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_manager_v1::ExtDataControlManagerV1 as Manager;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_offer_v1::{
    self as offer, ExtDataControlOfferV1 as Offer,
};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_source_v1::{
    self as source, ExtDataControlSourceV1 as Source,
};

/// The protocol's name, by which a compositor announces it.
pub const PROTOCOL: &str = "ext_data_control_manager_v1";

// One file for both protocols, on purpose: see its first lines.
#[allow(clippy::duplicate_mod)]
#[path = "data_control.rs"]
mod data_control;

pub use data_control::run;
