//! Screen capture through a Wayland protocol: the standard ext-image-copy-capture-v1, or wlr-screencopy, which
//! wlroots compositors offered before it. With both the compositor copies the screen into a buffer of ours,
//! and only when the screen has changed, so a still screen costs nothing. With the standard protocol that
//! buffer is on the GPU where the encoder takes its pictures there and the compositor can copy into it (see
//! `offer_gpu_buffer`): the picture then never passes through memory.

mod ext;
mod wlr;

use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop, event_created_child};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::{
    self as params, ZwpLinuxBufferParamsV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;
use wayland_protocols_wlr::output_power_management::v1::client::zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1;
use wayland_protocols_wlr::output_power_management::v1::client::zwlr_output_power_v1::{
    self as power, ZwlrOutputPowerV1,
};

use super::memory::Memory;
use crate::platform::linux::compositor::{self, bind, bind_versions};
use crate::stream::Stream;

/// Whether a compositor that offers these protocols can be captured from here.
pub fn offered(protocols: &[String]) -> bool {
    protocols.iter().any(|name| name == ext::PROTOCOL || name == wlr::PROTOCOL)
}

/// Captures the chosen output until the stream is no longer wanted or the compositor ends the capture.
pub fn run(mut stream: Stream) -> Result<(), String> {
    while stream.wanted() {
        stream = capture(stream)?;
    }
    stream.ended.take().unwrap_or(Ok(()))
}

fn capture(stream: Stream) -> Result<Stream, String> {
    let (_connection, globals, mut events) = compositor::connect::<Capture>()?;
    let queue = events.handle();
    let shm: WlShm = bind(&globals, &queue)?;
    // Buffers on the GPU are made with modifiers, which linux-dmabuf takes from version 3 on.
    let dmabuf = bind_versions::<ZwpLinuxDmabufV1, _>(&globals, &queue, 3..=3).ok();
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
        dmabuf,
        protocol,
        pixels: None,
        making: false,
        gpu_refused: false,
        awake: power.is_none(),
        frame_pending: false,
        requested: Instant::now() - Duration::from_secs(1),
    };
    capture.request_frame(&queue);
    while capture.stream.wanted() {
        let patience = capture.stream.patience();
        compositor::turn(&mut events, &mut capture, None, patience)?;
        if capture.stream.refine() {
            break; // each new capture session supplies the first frame without waiting for damage
        }
    }
    Ok(capture.stream)
}

enum Protocol {
    Standard(ext::Session),
    Wlroots(wlr::Screencopy),
}

/// A buffer the compositor copies the screen into, and where it is.
struct Pixels {
    buffer: WlBuffer,
    store: Store,
    width: usize,
    height: usize,
    stride: usize,
}

/// Memory shared with the compositor, or a buffer on the GPU of the encoder opened as `opened` (see
/// `Stream::gpu_picture`).
enum Store {
    Memory(Memory),
    Gpu(u64),
}

/// What a buffer on the GPU that the compositor is asked to make is: the encoder's, of this size.
struct Asked {
    opened: u64,
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
    dmabuf: Option<ZwpLinuxDmabufV1>,
    protocol: Protocol,
    pixels: Option<Pixels>,
    /// Whether the compositor is making a buffer on the GPU, and whether it could not use one: the capture
    /// then stays in memory.
    making: bool,
    gpu_refused: bool,
    /// Whether the display is known to be on. Frames are only requested then: one requested from a sleeping
    /// display never arrives, even after it wakes.
    awake: bool,
    frame_pending: bool,
    requested: Instant,
}

impl Capture {
    /// Makes sure there is a buffer of the size and kind the compositor announced: on the GPU where it offers
    /// a layout of `on_gpu` (DRM formats and modifiers) that the encoder's is, else in memory.
    fn provide_buffer(
        &mut self,
        queue: &QueueHandle<Self>,
        (width, height, stride): (usize, usize, usize),
        format: WEnum<wl_shm::Format>,
        on_gpu: &[(u32, u64)],
    ) -> Result<(), String> {
        // A buffer on the GPU has the encoder's rows, which need not be the compositor's.
        let fits = |pixels: &Pixels| {
            (pixels.width, pixels.height) == (width, height)
                && (matches!(pixels.store, Store::Gpu(_)) || pixels.stride == stride)
        };
        if self.pixels.as_ref().is_some_and(fits) || self.making || self.offer_gpu_buffer(queue, width, height, on_gpu)
        {
            return Ok(());
        }
        // Four bytes a pixel: blue, green, red and one more, or with red first, as renderers on a GPU often read.
        use wl_shm::Format::{Abgr8888, Argb8888, Xbgr8888, Xrgb8888};
        let WEnum::Value(format @ (Argb8888 | Xrgb8888 | Abgr8888 | Xbgr8888)) = format else {
            return Err(format!("unsupported capture pixel format {format:?}"));
        };
        self.stream.red_first = matches!(format, Abgr8888 | Xbgr8888);
        let memory = Memory::new(stride * height)?;
        let pool = self.shm.create_pool(memory.file.as_fd(), (stride * height) as i32, queue, ());
        let buffer = pool.create_buffer(0, width as i32, height as i32, stride as i32, format, queue, ());
        pool.destroy();
        self.pixels = Some(Pixels { buffer, store: Store::Memory(memory), width, height, stride });
        Ok(())
    }

    /// Asks the compositor to make a buffer of the encoder's on the GPU, where the encoder has one and the
    /// compositor offered its layout. True if asked: the buffer comes with the answer (see the params' events).
    fn offer_gpu_buffer(
        &mut self,
        queue: &QueueHandle<Self>,
        width: usize,
        height: usize,
        on_gpu: &[(u32, u64)],
    ) -> bool {
        let Some(dmabuf) = self.dmabuf.as_ref().filter(|_| !self.gpu_refused) else { return false };
        let Some((gpu, opened)) = self.stream.gpu_buffer(width, height) else { return false };
        // The file is the compositor's copy once sent; this one closes when it goes.
        let file = unsafe { OwnedFd::from_raw_fd(gpu.fd) };
        if !on_gpu.contains(&(gpu.format, gpu.modifier)) {
            return false;
        }
        let asked = Asked { opened, width, height, stride: gpu.pitch as usize };
        let params = dmabuf.create_params(queue, asked);
        params.add(file.as_fd(), 0, gpu.offset, gpu.pitch, (gpu.modifier >> 32) as u32, gpu.modifier as u32);
        params.create(width as i32, height as i32, gpu.format, params::Flags::empty());
        self.making = true;
        true
    }

    /// Provides a buffer anew, as the standard protocol's session last asked for one, and asks for a frame:
    /// after the one there was belonged to an encoder that is gone, or the GPU's could not be used.
    fn provide_again(&mut self, queue: &QueueHandle<Self>) {
        let Protocol::Standard(session) = &self.protocol else { return };
        let (size, format, on_gpu) =
            (session.size, session.format.unwrap_or(WEnum::Unknown(0)), session.on_gpu.clone());
        match self.provide_buffer(queue, (size.0, size.1, size.0 * 4), format, &on_gpu) {
            Ok(()) => self.request_frame(queue),
            Err(reason) => self.fail(reason),
        }
    }

    /// The compositor could not copy into the buffer on the GPU: on in memory.
    fn refuse_gpu(&mut self, queue: &QueueHandle<Self>) {
        self.gpu_refused = true;
        self.pixels = None;
        self.provide_again(queue);
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
        match &mut pixels.store {
            Store::Memory(memory) => {
                if upside_down {
                    flip(memory.bytes(), pixels.stride, pixels.height);
                }
                self.stream.picture(memory.bytes(), pixels.stride, pixels.width, pixels.height);
            }
            // A buffer of an encoder that is gone makes way for one of the new encoder's.
            &mut Store::Gpu(opened) => {
                if !self.stream.gpu_picture(opened) {
                    self.pixels = None;
                }
            }
        }
    }

    /// A frame is over, with a picture or without: on to the next, in a new buffer where the last went.
    fn frame_done(&mut self, queue: &QueueHandle<Self>) {
        self.frame_pending = false;
        if self.stream.ended.is_some() {
            return;
        }
        if self.pixels.is_none() && !self.making {
            self.provide_again(queue);
        } else {
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

impl Dispatch<ZwpLinuxBufferParamsV1, Asked> for Capture {
    fn event(
        state: &mut Self,
        params: &ZwpLinuxBufferParamsV1,
        event: params::Event,
        asked: &Asked,
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        state.making = false;
        params.destroy();
        match event {
            params::Event::Created { buffer } => {
                let Asked { opened, width, height, stride } = *asked;
                warn!("the screen is copied on the GPU, at {width}x{height}");
                state.pixels = Some(Pixels { buffer, store: Store::Gpu(opened), width, height, stride });
                state.request_frame(queue);
            }
            _ => state.refuse_gpu(queue),
        }
    }

    event_created_child!(Capture, ZwpLinuxBufferParamsV1, [
        params::EVT_CREATED_OPCODE => (WlBuffer, ()),
    ]);
}

delegate_noop!(Capture: ignore WlShm);
delegate_noop!(Capture: ignore ZwpLinuxDmabufV1);
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
