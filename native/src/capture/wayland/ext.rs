//! The standard protocol, ext-image-copy-capture-v1: a session says what buffer it wants, and each frame is
//! then copied into the one attached to it.

use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_shm;
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1;
use wayland_protocols::ext::image_capture_source::v1::client::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_frame_v1::{
    self as frame, ExtImageCopyCaptureFrameV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::{
    ExtImageCopyCaptureManagerV1, Options,
};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_session_v1::{
    self as session, ExtImageCopyCaptureSessionV1,
};

use super::{Capture, Pixels, Protocol};
use crate::compositor::bind;

/// The protocol's name, by which a compositor announces it.
pub const PROTOCOL: &str = "ext_image_copy_capture_manager_v1";

pub struct Session {
    session: ExtImageCopyCaptureSessionV1,
    /// The buffer the session wants, as far as it has said.
    size: (usize, usize),
    format: Option<WEnum<wl_shm::Format>>,
}

impl Session {
    pub fn start(
        globals: &GlobalList,
        queue: &QueueHandle<Capture>,
        output: &WlOutput,
        cursor: bool,
    ) -> Result<Self, String> {
        let sources: ExtOutputImageCaptureSourceManagerV1 = bind(globals, queue)?;
        let manager: ExtImageCopyCaptureManagerV1 = bind(globals, queue)?;
        let source = sources.create_source(output, queue, ());
        let options = if cursor { Options::PaintCursors } else { Options::empty() };
        Ok(Self { session: manager.create_session(&source, options, queue, ()), size: (0, 0), format: None })
    }

    pub fn request(&self, pixels: &Pixels, queue: &QueueHandle<Capture>) {
        let frame = self.session.create_frame(queue, ());
        frame.attach_buffer(&pixels.buffer);
        frame.damage_buffer(0, 0, pixels.width as i32, pixels.height as i32);
        frame.capture();
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for Capture {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: session::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let Protocol::Standard(wanted) = &mut state.protocol else { return };
        match event {
            session::Event::BufferSize { width, height } => wanted.size = (width as usize, height as usize),
            // Of the formats it offers, the first is taken.
            session::Event::ShmFormat { format } if wanted.format.is_none() => wanted.format = Some(format),
            session::Event::Done => {
                let ((width, height), format) = (wanted.size, wanted.format.unwrap_or(WEnum::Unknown(0)));
                match state.provide_buffer(queue, (width, height, width * 4), format) {
                    Ok(()) => state.request_frame(queue),
                    Err(reason) => state.fail(reason),
                }
            }
            session::Event::Stopped => state.fail("the compositor stopped the capture"),
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for Capture {
    fn event(
        state: &mut Self,
        frame: &ExtImageCopyCaptureFrameV1,
        event: frame::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        match event {
            frame::Event::Ready => state.frame_ready(false),
            // The buffer no longer fits; the session announces the new size and a new one is made.
            frame::Event::Failed { reason: WEnum::Value(frame::FailureReason::BufferConstraints) } => {}
            frame::Event::Failed { .. } => state.fail("the compositor could not capture a frame"),
            _ => return,
        }
        frame.destroy();
        state.frame_done(queue);
    }
}

delegate_noop!(Capture: ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(Capture: ExtImageCaptureSourceV1);
delegate_noop!(Capture: ExtImageCopyCaptureManagerV1);
