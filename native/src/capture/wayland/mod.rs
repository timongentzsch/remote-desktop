//! Screen capture through a Wayland protocol: the standard ext-image-copy-capture-v1, or wlr-screencopy, which
//! wlroots compositors offered before it. With both the compositor copies the screen into a buffer of ours,
//! and only when the screen has changed, so a still screen costs nothing.

mod ext;
mod wlr;

use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols_wlr::output_power_management::v1::client::zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1;
use wayland_protocols_wlr::output_power_management::v1::client::zwlr_output_power_v1::{
    self as power, ZwlrOutputPowerV1,
};

use super::memory::Memory;
use crate::compositor::{self, bind};
use crate::stream::Stream;

/// Whether a compositor that offers these protocols can be captured from here.
pub fn offered(protocols: &[String]) -> bool {
    protocols.iter().any(|name| name == ext::PROTOCOL || name == wlr::PROTOCOL)
}

/// Captures the chosen output until the stream is no longer wanted or the compositor ends the capture.
pub fn run(stream: Stream) -> Result<(), String> {
    let (_connection, globals, mut events) = compositor::connect::<Capture>()?;
    let queue = events.handle();
    let shm: WlShm = bind(&globals, &queue)?;
    let output = compositor::output(&globals, &queue, stream.settings.screen.as_deref())?;

    // A sleeping display produces no frames, so it is switched on first where the compositor allows that.
    let power = bind::<ZwlrOutputPowerManagerV1, _>(&globals, &queue)
        .ok()
        .map(|manager| manager.get_output_power(&output, &queue, ()));
    let cursor = stream.settings.cursor;
    let protocol = match ext::Session::start(&globals, &queue, &output, cursor) {
        Ok(session) => Protocol::Standard(session),
        Err(_) => Protocol::Wlroots(wlr::Screencopy::start(&globals, &queue, output, cursor)?),
    };

    let mut capture = Capture {
        stream,
        shm,
        protocol,
        pixels: None,
        awake: power.is_none(),
        frame_pending: false,
        requested: Instant::now() - Duration::from_secs(1),
    };
    capture.request_frame(&queue);
    while capture.stream.wanted() {
        let patience = capture.stream.patience();
        compositor::turn(&mut events, &mut capture, None, patience)?;
        capture.stream.refine();
    }
    capture.stream.ended.take().unwrap_or(Ok(()))
}

enum Protocol {
    Standard(ext::Session),
    Wlroots(wlr::Screencopy),
}

/// A buffer the compositor copies the screen into.
struct Pixels {
    buffer: WlBuffer,
    memory: Memory,
    width: usize,
    height: usize,
    stride: usize,
}

impl Drop for Pixels {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

struct Capture {
    stream: Stream,
    shm: WlShm,
    protocol: Protocol,
    pixels: Option<Pixels>,
    /// Whether the display is known to be on. Frames are only requested then: one requested from a sleeping
    /// display never arrives, even after it wakes.
    awake: bool,
    frame_pending: bool,
    requested: Instant,
}

impl Capture {
    /// Makes sure there is a buffer of the size and kind the compositor announced.
    fn provide_buffer(
        &mut self,
        queue: &QueueHandle<Self>,
        (width, height, stride): (usize, usize, usize),
        format: WEnum<wl_shm::Format>,
    ) -> Result<(), String> {
        let fits = |pixels: &Pixels| (pixels.width, pixels.height, pixels.stride) == (width, height, stride);
        if self.pixels.as_ref().is_some_and(fits) {
            return Ok(());
        }
        // Both hold blue, green, red and a fourth byte per pixel, which is what the stream takes.
        let WEnum::Value(format @ (wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888)) = format else {
            return Err(format!("unsupported capture pixel format {format:?}"));
        };
        let memory = Memory::new(stride * height)?;
        let pool = self.shm.create_pool(memory.file.as_fd(), (stride * height) as i32, queue, ());
        let buffer = pool.create_buffer(0, width as i32, height as i32, stride as i32, format, queue, ());
        pool.destroy();
        self.pixels = Some(Pixels { buffer, memory, width, height, stride });
        Ok(())
    }

    /// Asks for the next frame, unless one is on its way or the display is asleep.
    fn request_frame(&mut self, queue: &QueueHandle<Self>) {
        if !self.awake || self.frame_pending {
            return;
        }
        // Hold the request back when the screen changes faster than the wanted frame rate.
        let due = self.requested + self.stream.frame_interval();
        let requested = match (&mut self.protocol, &self.pixels) {
            (Protocol::Standard(session), Some(pixels)) => {
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
                session.request(pixels, queue);
                true
            }
            // The standard protocol first says what buffer it wants; the request follows that.
            (Protocol::Standard(_), None) => false,
            (Protocol::Wlroots(screencopy), _) => {
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
                screencopy.request(queue);
                true
            }
        };
        if requested {
            self.requested = Instant::now();
            self.frame_pending = true;
        }
    }

    /// The compositor has copied the screen into the buffer, with the rows from the bottom up if `upside_down`.
    fn frame_ready(&mut self, upside_down: bool) {
        let Some(pixels) = &mut self.pixels else { return };
        if upside_down {
            flip(pixels.memory.bytes(), pixels.stride, pixels.height);
        }
        self.stream.picture(pixels.memory.bytes(), pixels.stride, pixels.width, pixels.height);
    }

    /// A frame is over, with a picture or without: on to the next.
    fn frame_done(&mut self, queue: &QueueHandle<Self>) {
        self.frame_pending = false;
        if self.stream.ended.is_none() {
            self.request_frame(queue);
        }
    }

    fn fail(&mut self, reason: impl Into<String>) {
        self.stream.ended = Some(Err(reason.into()));
    }
}

/// Turns a picture whose rows run from the bottom up the right way round.
fn flip(pixels: &mut [u8], stride: usize, height: usize) {
    for row in 0..height / 2 {
        let (upper, lower) = pixels.split_at_mut((height - 1 - row) * stride);
        upper[row * stride..][..stride].swap_with_slice(&mut lower[..stride]);
    }
}

impl Dispatch<ZwlrOutputPowerV1, ()> for Capture {
    fn event(
        state: &mut Self,
        output: &ZwlrOutputPowerV1,
        event: power::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        match event {
            power::Event::Mode { mode: WEnum::Value(power::Mode::On) } => {
                state.awake = true;
                state.request_frame(queue);
            }
            power::Event::Mode { .. } => {
                state.awake = false;
                output.set_mode(power::Mode::On);
            }
            // The compositor will not report or change this output's power: carry on as if it were on.
            _ => {
                state.awake = true;
                state.request_frame(queue);
            }
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Capture {
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

delegate_noop!(Capture: ignore WlShm);
delegate_noop!(Capture: ignore WlBuffer);
delegate_noop!(Capture: ignore WlOutput);
delegate_noop!(Capture: WlShmPool);
delegate_noop!(Capture: ZwlrOutputPowerManagerV1);

#[cfg(test)]
mod tests {
    #[test]
    fn flipping_reverses_the_rows() {
        // Three rows of two bytes, the middle one staying where it is.
        let mut pixels = [1, 1, 2, 2, 3, 3];
        super::flip(&mut pixels, 2, 3);
        assert_eq!(pixels, [3, 3, 2, 2, 1, 1]);
    }
}
