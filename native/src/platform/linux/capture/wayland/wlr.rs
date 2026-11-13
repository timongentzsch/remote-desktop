//! The protocol wlroots compositors had first, wlr-screencopy: every frame is asked for on its own, says what
//! buffer it wants, and is then copied into the one it is given.

use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::{self as frame, ZwlrScreencopyFrameV1};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

use super::{Capture, Protocol};
use crate::platform::linux::compositor::bind_versions;

/// The protocol's name, by which a compositor announces it.
pub const PROTOCOL: &str = "zwlr_screencopy_manager_v1";
/// From its third version on a frame announces several kinds of buffer and then says that it is done.
const ANNOUNCES_SEVERAL: u32 = 3;
/// How many frames in a row may fail before the capture gives up. A single one can, when the output changes
/// under it.
const FAILURES_TOLERATED: u32 = 10;

pub struct Screencopy {
    manager: ZwlrScreencopyManagerV1,
    output: WlOutput,
    cursor: bool,
    /// The first picture is wanted at once; later ones only when the screen has changed.
    copied_before: bool,
    /// Whether the frame on its way has its rows from the bottom up.
    upside_down: bool,
    failures: u32,
}

impl Screencopy {
    pub fn start(
        globals: &GlobalList,
        queue: &QueueHandle<Capture>,
        output: WlOutput,
        cursor: bool,
    ) -> Result<Self, String> {
        // Copying only when the screen has changed came with the second version.
        let manager = bind_versions(globals, queue, 2..=ANNOUNCES_SEVERAL)?;
        Ok(Self { manager, output, cursor, copied_before: false, upside_down: false, failures: 0 })
    }

    pub fn request(&mut self, queue: &QueueHandle<Capture>) {
        self.upside_down = false;
        self.manager.capture_output(self.cursor as i32, &self.output, queue, ());
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for Capture {
    fn event(
        state: &mut Self,
        frame: &ZwlrScreencopyFrameV1,
        event: frame::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let copy = |state: &mut Self| {
            let (Protocol::Wlroots(screencopy), Some(pixels)) = (&mut state.protocol, &state.pixels) else { return };
            if screencopy.copied_before {
                frame.copy_with_damage(&pixels.buffer)
            } else {
                frame.copy(&pixels.buffer)
            }
            screencopy.copied_before = true;
        };
        match event {
            frame::Event::Buffer { format, width, height, stride } => {
                match state.provide_buffer(queue, (width as usize, height as usize, stride as usize), format) {
                    Ok(()) if frame.version() < ANNOUNCES_SEVERAL => copy(state),
                    Ok(()) => {}
                    Err(reason) => state.fail(reason),
                }
            }
            frame::Event::BufferDone => copy(state),
            frame::Event::Flags { flags } => {
                if let Protocol::Wlroots(screencopy) = &mut state.protocol {
                    screencopy.upside_down =
                        matches!(flags, WEnum::Value(flags) if flags.contains(frame::Flags::YInvert));
                }
            }
            frame::Event::Ready { .. } | frame::Event::Failed => {
                let Protocol::Wlroots(screencopy) = &mut state.protocol else { return };
                if matches!(event, frame::Event::Failed) {
                    screencopy.failures += 1;
                    // The next frame is to come at once again, not when something changes.
                    screencopy.copied_before = false;
                    if screencopy.failures > FAILURES_TOLERATED {
                        state.fail("the compositor could not capture a frame");
                    }
                } else {
                    screencopy.failures = 0;
                    let upside_down = screencopy.upside_down;
                    state.frame_ready(upside_down);
                }
                frame.destroy();
                state.frame_done(queue);
            }
            _ => {}
        }
    }
}

delegate_noop!(Capture: ZwlrScreencopyManagerV1);
