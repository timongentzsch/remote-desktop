//! Screen capture through the compositor's ext-image-copy-capture-v1 protocol, encoded into FLV video tags.
//!
//! The compositor hands over a frame only when the screen changed, so a still screen costs nothing.

use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1;
use wayland_protocols::ext::image_capture_source::v1::client::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_frame_v1::{self as frame, ExtImageCopyCaptureFrameV1};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_session_v1::{self as session, ExtImageCopyCaptureSessionV1};

use wayland_protocols_wlr::output_power_management::v1::client::zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1;
use wayland_protocols_wlr::output_power_management::v1::client::zwlr_output_power_v1::{self as power, ZwlrOutputPowerV1};

use crate::convert;
use crate::encode::Encoder;

pub struct Settings {
    pub kbps: u32,
    pub fps: u32,
    /// Paint the pointer into the picture. Off when the client draws its own.
    pub cursor: bool,
    /// Keep full colour resolution (4:4:4) instead of 4:2:0.
    pub full_chroma: bool,
    pub half_size: bool,
}

/// What a session and its capture exchange while the capture runs.
#[derive(Default)]
pub struct Shared {
    /// Set by the session to end the capture.
    pub stop: AtomicBool,
    /// The bitrate cap in kbit/s the client now wants; 0 until it asks for a change.
    pub kbps: AtomicU32,
    /// How long converting and encoding the latest frame took, in microseconds.
    pub encode_micros: AtomicU32,
}

/// For how many seconds at most a still picture is encoded again to bring it up to the quality target.
const REFINE_SECONDS: u32 = 20;
/// How many frames in a row have to add nothing before a still picture counts as finished. A few are not
/// enough: while the bitrate cap holds the encoder back, frames that add detail alternate with ones that add none.
pub const SETTLED_FRAMES: u32 = 20;

/// Receives each piece of the stream; returns false once nobody is listening any more.
pub type Sink = Box<dyn FnMut(&[u8]) -> bool + Send>;

/// Captures the first output until `shared.stop` is set, the sink declines a piece, or the compositor ends it.
pub fn run(settings: Settings, shared: Arc<Shared>, sink: Sink) -> Result<(), String> {
    let connection = Connection::connect_to_env().map_err(|error| format!("no Wayland display: {error}"))?;
    let (globals, mut events) = registry_queue_init::<Capture>(&connection).map_err(|error| error.to_string())?;
    let queue = events.handle();
    let missing = |error| format!("the compositor lacks a protocol this needs: {error}");
    let shm: WlShm = globals.bind(&queue, 1..=1, ()).map_err(missing)?;
    let output: WlOutput = globals.bind(&queue, 1..=1, ()).map_err(missing)?;
    let sources: ExtOutputImageCaptureSourceManagerV1 = globals.bind(&queue, 1..=1, ()).map_err(missing)?;
    let manager: ExtImageCopyCaptureManagerV1 = globals.bind(&queue, 1..=1, ()).map_err(missing)?;

    // A sleeping display produces no frames, so it is switched on first where the compositor allows that.
    let power = globals.bind::<ZwlrOutputPowerManagerV1, _, _>(&queue, 1..=1, ()).ok().map(|manager| manager.get_output_power(&output, &queue, ()));
    let source = sources.create_source(&output, &queue, ());
    let options = if settings.cursor { Options::PaintCursors } else { Options::empty() };
    let session = manager.create_session(&source, options, &queue, ());

    let now = Instant::now();
    let mut capture = Capture {
        settings,
        shared: shared.clone(),
        shm,
        session,
        size: (0, 0),
        format: None,
        pixels: None,
        scaled: Vec::new(),
        encoder: None,
        awake: power.is_none(),
        refinements_left: 0,
        fine_frames: 0,
        frames_sent: 0,
        frame_pending: false,
        started: now,
        requested: now - Duration::from_secs(1),
        sink,
        wrote_header: false,
        ended: None,
    };
    let lost = |error| format!("lost the Wayland connection: {error}");
    while !shared.stop.load(Ordering::Relaxed) && capture.ended.is_none() {
        events.flush().map_err(lost)?;
        // Wait for events, but look at `stop` a few times a second: a still screen sends nothing. While a still
        // picture is being refined, wait no longer than one frame.
        let wait = if capture.refinements_left > 0 { 1000 / capture.settings.fps as i32 } else { 250 };
        if let Some(guard) = events.prepare_read() {
            let mut poll = libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 };
            if unsafe { libc::poll(&mut poll, 1, wait) } > 0 {
                guard.read().map_err(lost)?;
            }
        }
        let sent = capture.frames_sent;
        events.dispatch_pending(&mut capture).map_err(|error| format!("lost the Wayland connection: {error}"))?;
        if capture.frames_sent == sent && capture.refinements_left > 0 {
            capture.refinements_left -= 1;
            capture.encode_picture(Instant::now());
        }
    }
    capture.ended.take().unwrap_or(Ok(()))
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

// The mapping is only read, and only by the thread that owns the capture.
unsafe impl Send for Pixels {}

impl Pixels {
    fn new(shm: &WlShm, queue: &QueueHandle<Capture>, width: usize, height: usize, format: wl_shm::Format) -> Result<Self, String> {
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
    settings: Settings,
    shared: Arc<Shared>,
    shm: WlShm,
    session: ExtImageCopyCaptureSessionV1,
    size: (usize, usize),
    format: Option<wl_shm::Format>,
    pixels: Option<Pixels>,
    /// The half-size copy of the screen, when the picture is scaled down.
    scaled: Vec<u8>,
    encoder: Option<Encoder>,
    /// Whether the display is known to be on. Frames are only requested then: one requested from a sleeping
    /// display never arrives, even after it wakes.
    awake: bool,
    /// A bitrate cap can force a frame out coarser than wanted. The screen may then stay still, and no further
    /// frame would ever improve it, so the same picture is encoded again until the encoder has nothing to add.
    refinements_left: u32,
    fine_frames: u32,
    frames_sent: u64,
    frame_pending: bool,
    started: Instant,
    requested: Instant,
    sink: Sink,
    wrote_header: bool,
    /// Set once the capture is over: Ok when the sink went away, Err with the reason otherwise.
    ended: Option<Result<(), String>>,
}

impl Capture {
    /// (Re)creates the buffer and encoder for the size the compositor announced.
    fn configure(&mut self, queue: &QueueHandle<Self>) -> Result<(), String> {
        let (width, height) = self.size;
        if self.pixels.as_ref().is_some_and(|pixels| (pixels.width, pixels.height) == (width, height)) {
            return Ok(());
        }
        let format = match self.format {
            Some(format @ (wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888)) => format,
            other => return Err(format!("unsupported capture pixel format {other:?}")),
        };
        self.pixels = Some(Pixels::new(&self.shm, queue, width, height, format)?);
        let scale = if self.settings.half_size { 2 } else { 1 };
        // 4:2:0 needs even dimensions; an odd last row or column is dropped.
        let (width, height) = (width / scale & !1, height / scale & !1);
        let mut encoder = Encoder::open(width, height, self.settings.fps, self.settings.kbps, self.settings.full_chroma)
            .ok_or("could not open the encoder")?;
        if !self.wrote_header {
            self.send(b"FLV\x01\x01\x00\x00\x00\x09\x00\x00\x00\x00");
            self.wrote_header = true;
        }
        let configuration = encoder.configuration();
        self.send_tag(true, 0, &configuration);
        self.encoder = Some(encoder);
        Ok(())
    }

    fn request_frame(&mut self, queue: &QueueHandle<Self>) {
        if !self.awake || self.frame_pending {
            return;
        }
        let Some(pixels) = &self.pixels else { return };
        // Hold the request back when the screen changes faster than the wanted frame rate.
        let due = self.requested + Duration::from_secs(1) / self.settings.fps;
        std::thread::sleep(due.saturating_duration_since(Instant::now()));
        let frame = self.session.create_frame(queue, ());
        frame.attach_buffer(&pixels.buffer);
        frame.damage_buffer(0, 0, pixels.width as i32, pixels.height as i32);
        frame.capture();
        self.requested = Instant::now();
        self.frame_pending = true;
    }

    fn encode(&mut self) {
        let (Some(pixels), Some(encoder)) = (&self.pixels, &mut self.encoder) else { return };
        let started = Instant::now();
        let wanted = self.shared.kbps.load(Ordering::Relaxed);
        if wanted != 0 && wanted != self.settings.kbps && encoder.set_bitrate(wanted) {
            self.settings.kbps = wanted;
        }
        let (width, height, full_chroma) = (encoder.width, encoder.height, encoder.full_chroma);
        let (mut source, mut stride) = (pixels.bytes(), pixels.width * 4);
        if self.settings.half_size {
            convert::halve(source, stride, pixels.width, pixels.height, &mut self.scaled);
            (source, stride) = (&self.scaled, pixels.width / 2 * 4);
        }
        convert::bgrx_to_yuv(source, stride, width, height, encoder.planes(), full_chroma);
        self.refinements_left = REFINE_SECONDS * self.settings.fps;
        self.encode_picture(started);
    }

    /// Encodes the picture currently in the encoder's planes and sends it.
    fn encode_picture(&mut self, started: Instant) {
        let Some(encoder) = &mut self.encoder else { return };
        if let Some(encoded) = encoder.encode() {
            let (unit, keyframe, settled) = (encoded.unit.to_vec(), encoded.keyframe, encoded.settled);
            self.shared.encode_micros.store(started.elapsed().as_micros() as u32, Ordering::Relaxed);
            self.fine_frames = if settled { self.fine_frames + 1 } else { 0 };
            if self.fine_frames >= SETTLED_FRAMES {
                self.refinements_left = 0;
            }
            self.frames_sent += 1;
            self.send_tag(keyframe, 1, &unit);
        }
    }

    /// Sends one FLV video tag. Packet type 0 carries the decoder configuration, 1 an access unit.
    fn send_tag(&mut self, keyframe: bool, packet_type: u8, payload: &[u8]) {
        let size = payload.len() as u32 + 5;
        let time = (self.started.elapsed().as_millis() as u32).to_be_bytes();
        let mut tag = Vec::with_capacity(payload.len() + 20);
        tag.push(9);
        tag.extend(&size.to_be_bytes()[1..]);
        tag.extend([time[1], time[2], time[3], time[0], 0, 0, 0]);
        tag.extend([if keyframe { 0x17 } else { 0x27 }, packet_type, 0, 0, 0]);
        tag.extend(payload);
        tag.extend((size + 11).to_be_bytes());
        self.send(&tag);
    }

    fn send(&mut self, bytes: &[u8]) {
        if !(self.sink)(bytes) {
            self.ended.get_or_insert(Ok(()));
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for Capture {
    fn event(state: &mut Self, _: &ExtImageCopyCaptureSessionV1, event: session::Event, _: &(), _: &Connection, queue: &QueueHandle<Self>) {
        match event {
            session::Event::BufferSize { width, height } => state.size = (width as usize, height as usize),
            session::Event::ShmFormat { format: WEnum::Value(format) } if state.format.is_none() => state.format = Some(format),
            session::Event::Done => match state.configure(queue) {
                Ok(()) => state.request_frame(queue),
                Err(reason) => state.ended = Some(Err(reason)),
            },
            session::Event::Stopped => state.ended = Some(Err("the compositor stopped the capture".into())),
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for Capture {
    fn event(state: &mut Self, frame: &ExtImageCopyCaptureFrameV1, event: frame::Event, _: &(), _: &Connection, queue: &QueueHandle<Self>) {
        match event {
            frame::Event::Ready => state.encode(),
            // The buffer no longer fits; the session announces the new size and `configure` replaces it.
            frame::Event::Failed { reason: WEnum::Value(frame::FailureReason::BufferConstraints) } => {}
            frame::Event::Failed { .. } => state.ended = Some(Err("the compositor could not capture a frame".into())),
            _ => return,
        }
        frame.destroy();
        state.frame_pending = false;
        if state.ended.is_none() {
            state.request_frame(queue);
        }
    }
}

impl Dispatch<ZwlrOutputPowerV1, ()> for Capture {
    fn event(state: &mut Self, output: &ZwlrOutputPowerV1, event: power::Event, _: &(), _: &Connection, queue: &QueueHandle<Self>) {
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
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as wayland_client::Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

delegate_noop!(Capture: ignore WlShm);
delegate_noop!(Capture: ignore WlBuffer);
delegate_noop!(Capture: ignore WlOutput);
delegate_noop!(Capture: WlShmPool);
delegate_noop!(Capture: ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(Capture: ExtImageCaptureSourceV1);
delegate_noop!(Capture: ExtImageCopyCaptureManagerV1);
delegate_noop!(Capture: ZwlrOutputPowerManagerV1);
