//! Screen capture through the ext-image-copy-capture-v1 protocol, which Hyprland, Sway and other wlroots
//! compositors offer.
//!
//! The compositor hands over a frame only when the screen changed, so a still screen costs nothing.

use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
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
use wayland_protocols_wlr::output_power_management::v1::client::zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1;
use wayland_protocols_wlr::output_power_management::v1::client::zwlr_output_power_v1::{
    self as power, ZwlrOutputPowerV1,
};

use crate::compositor::{self, bind, lost};
use crate::stream::Stream;

/// The protocol's name, by which a compositor announces it.
pub const PROTOCOL: &str = "ext_image_copy_capture_manager_v1";

/// Captures the first output until the stream is no longer wanted or the compositor ends the capture.
pub fn run(stream: Stream) -> Result<(), String> {
    let (_connection, globals, mut events) = compositor::connect::<Capture>()?;
    let queue = events.handle();
    let shm: WlShm = bind(&globals, &queue)?;
    let output: WlOutput = bind(&globals, &queue)?;
    let sources: ExtOutputImageCaptureSourceManagerV1 = bind(&globals, &queue)?;
    let manager: ExtImageCopyCaptureManagerV1 = bind(&globals, &queue)?;

    // A sleeping display produces no frames, so it is switched on first where the compositor allows that.
    let power = bind::<ZwlrOutputPowerManagerV1, _>(&globals, &queue)
        .ok()
        .map(|manager| manager.get_output_power(&output, &queue, ()));
    let source = sources.create_source(&output, &queue, ());
    let options = if stream.settings.cursor { Options::PaintCursors } else { Options::empty() };
    let session = manager.create_session(&source, options, &queue, ());

    let mut capture = Capture {
        stream,
        shm,
        session,
        size: (0, 0),
        format: None,
        pixels: None,
        awake: power.is_none(),
        frame_pending: false,
        requested: Instant::now() - Duration::from_secs(1),
    };
    while capture.stream.wanted() {
        events.flush().map_err(lost)?;
        if let Some(guard) = events.prepare_read() {
            let mut poll = libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 };
            if unsafe { libc::poll(&mut poll, 1, capture.stream.patience().as_millis() as i32) } > 0 {
                guard.read().map_err(lost)?;
            }
        }
        events.dispatch_pending(&mut capture).map_err(lost)?;
        capture.stream.refine();
    }
    capture.stream.ended.take().unwrap_or(Ok(()))
}

/// A shared-memory buffer the compositor copies the screen into.
struct Pixels {
    buffer: WlBuffer,
    map: *const u8,
    length: usize,
    width: usize,
    height: usize,
    _memory: OwnedFd,
}

impl Pixels {
    fn new(
        shm: &WlShm,
        queue: &QueueHandle<Capture>,
        width: usize,
        height: usize,
        format: wl_shm::Format,
    ) -> Result<Self, String> {
        let length = width * height * 4;
        let (memory, map) = unsafe {
            let fd = libc::memfd_create(c"remote-desktop".as_ptr(), libc::MFD_CLOEXEC);
            if fd < 0 {
                return Err("could not allocate the capture buffer".into());
            }
            let memory = OwnedFd::from_raw_fd(fd);
            if libc::ftruncate(fd, length as libc::off_t) < 0 {
                return Err("could not size the capture buffer".into());
            }
            let map = libc::mmap(std::ptr::null_mut(), length, libc::PROT_READ, libc::MAP_SHARED, fd, 0);
            if map == libc::MAP_FAILED {
                return Err("could not map the capture buffer".into());
            }
            (memory, map as *const u8)
        };
        let pool = shm.create_pool(memory.as_fd(), length as i32, queue, ());
        let buffer = pool.create_buffer(0, width as i32, height as i32, width as i32 * 4, format, queue, ());
        pool.destroy();
        Ok(Self { buffer, map, length, width, height, _memory: memory })
    }

    fn bytes(&self) -> &[u8] {
        // The compositor only writes between a frame's capture request and its ready event; we read after ready.
        unsafe { std::slice::from_raw_parts(self.map, self.length) }
    }
}

impl Drop for Pixels {
    fn drop(&mut self) {
        self.buffer.destroy();
        unsafe { libc::munmap(self.map as *mut libc::c_void, self.length) };
    }
}

struct Capture {
    stream: Stream,
    shm: WlShm,
    session: ExtImageCopyCaptureSessionV1,
    size: (usize, usize),
    format: Option<wl_shm::Format>,
    pixels: Option<Pixels>,
    /// Whether the display is known to be on. Frames are only requested then: one requested from a sleeping
    /// display never arrives, even after it wakes.
    awake: bool,
    frame_pending: bool,
    requested: Instant,
}

impl Capture {
    /// (Re)creates the buffer for the size the compositor announced.
    fn configure(&mut self, queue: &QueueHandle<Self>) -> Result<(), String> {
        let (width, height) = self.size;
        if self.pixels.as_ref().is_some_and(|pixels| (pixels.width, pixels.height) == (width, height)) {
            return Ok(());
        }
        // Both hold blue, green, red and a fourth byte per pixel, which is what the stream takes.
        let format = match self.format {
            Some(format @ (wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888)) => format,
            other => return Err(format!("unsupported capture pixel format {other:?}")),
        };
        self.pixels = Some(Pixels::new(&self.shm, queue, width, height, format)?);
        Ok(())
    }

    fn request_frame(&mut self, queue: &QueueHandle<Self>) {
        if !self.awake || self.frame_pending {
            return;
        }
        let Some(pixels) = &self.pixels else { return };
        // Hold the request back when the screen changes faster than the wanted frame rate.
        let due = self.requested + self.stream.frame_interval();
        std::thread::sleep(due.saturating_duration_since(Instant::now()));
        let frame = self.session.create_frame(queue, ());
        frame.attach_buffer(&pixels.buffer);
        frame.damage_buffer(0, 0, pixels.width as i32, pixels.height as i32);
        frame.capture();
        self.requested = Instant::now();
        self.frame_pending = true;
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
        match event {
            session::Event::BufferSize { width, height } => state.size = (width as usize, height as usize),
            session::Event::ShmFormat { format: WEnum::Value(format) } if state.format.is_none() => {
                state.format = Some(format)
            }
            session::Event::Done => match state.configure(queue) {
                Ok(()) => state.request_frame(queue),
                Err(reason) => state.stream.ended = Some(Err(reason)),
            },
            session::Event::Stopped => state.stream.ended = Some(Err("the compositor stopped the capture".into())),
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
            frame::Event::Ready => {
                if let Some(pixels) = &state.pixels {
                    state.stream.picture(pixels.bytes(), pixels.width * 4, pixels.width, pixels.height);
                }
            }
            // The buffer no longer fits; the session announces the new size and `configure` replaces it.
            frame::Event::Failed { reason: WEnum::Value(frame::FailureReason::BufferConstraints) } => {}
            frame::Event::Failed { .. } => {
                state.stream.ended = Some(Err("the compositor could not capture a frame".into()))
            }
            _ => return,
        }
        frame.destroy();
        state.frame_pending = false;
        if state.stream.ended.is_none() {
            state.request_frame(queue);
        }
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
delegate_noop!(Capture: ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(Capture: ExtImageCaptureSourceV1);
delegate_noop!(Capture: ExtImageCopyCaptureManagerV1);
delegate_noop!(Capture: ZwlrOutputPowerManagerV1);
