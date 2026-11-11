//! Turns pictures of the screen into the video stream: each one is encoded, a still one is refined, and the
//! result is wrapped in FLV video tags for the sink. Where the pictures come from is the capture's business.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

mod convert;
mod encode;

pub use encode::{PROBE, probe};

use crate::capture::{Settings, Shared, Sink};
use encode::Encoder;

/// How many times a second a still picture is encoded again to bring it up to the quality target. Far fewer
/// than the frame rate, so that each of these frames may take a correspondingly larger share of the bitrate cap:
/// with a frame's ordinary share an encoder soon finds nothing it can add.
pub const REFINE_FPS: u32 = 10;
/// For how many seconds at most a still picture is refined.
const REFINE_SECONDS: u32 = 20;
/// How many refinements in a row have to add nothing before the picture counts as finished.
pub const SETTLED_FRAMES: u32 = 3;
const REFINE_INTERVAL: Duration = Duration::from_millis(1000 / REFINE_FPS as u64);

pub struct Stream {
    pub settings: Settings,
    shared: Arc<Shared>,
    sink: Sink,
    /// The size of the screen the encoder is open for.
    screen: (usize, usize),
    /// The half-size copy of the screen, when the picture is scaled down.
    scaled: Vec<u8>,
    encoder: Option<Encoder>,
    /// The bitrate cap in kbit/s and the frames a second the encoder was last told.
    rate: (u32, u32),
    /// A bitrate cap can force a frame out coarser than wanted. The screen may then stay still, and no further
    /// frame would ever improve it, so the same picture is encoded again until the encoder has nothing to add.
    refinements_left: u32,
    fine_frames: u32,
    /// When the latest frame, new or refined, was encoded.
    encoded: Instant,
    started: Instant,
    wrote_header: bool,
    /// Set once the stream is over: Ok when the sink went away, Err with the reason otherwise.
    pub ended: Option<Result<(), String>>,
}

impl Stream {
    pub fn new(settings: Settings, shared: Arc<Shared>, sink: Sink) -> Self {
        let now = Instant::now();
        Self {
            settings,
            shared,
            sink,
            screen: (0, 0),
            scaled: Vec::new(),
            encoder: None,
            rate: (0, 0),
            refinements_left: 0,
            fine_frames: 0,
            encoded: now,
            started: now,
            wrote_header: false,
            ended: None,
        }
    }

    /// Whether the capture should go on: nobody asked it to stop and the stream has not ended.
    pub fn wanted(&self) -> bool {
        !self.shared.stop.load(Ordering::Relaxed) && self.ended.is_none()
    }

    /// The time between two frames at the wanted frame rate.
    pub fn frame_interval(&self) -> Duration {
        Duration::from_secs(1) / self.settings.fps
    }

    /// How long the capture may wait for the screen to change before it calls `refine` and looks at `wanted`
    /// again: a still screen sends nothing, so not long, and no longer than until the next refinement is due.
    pub fn patience(&self) -> Duration {
        let refinement = if self.refinements_left > 0 {
            REFINE_INTERVAL.saturating_sub(self.encoded.elapsed())
        } else {
            Duration::MAX
        };
        refinement.min(Duration::from_millis(250))
    }

    /// Encodes a new picture of the screen. `pixels` holds its rows, `stride` bytes apart, each pixel as blue,
    /// green, red and a fourth byte that is ignored.
    pub fn picture(&mut self, pixels: &[u8], stride: usize, width: usize, height: usize) {
        let started = Instant::now();
        if (self.encoder.is_none() || self.screen != (width, height))
            && let Err(reason) = self.open(width, height)
        {
            self.ended = Some(Err(reason));
            return;
        }
        let Some(encoder) = &mut self.encoder else { return };
        let (mut source, mut stride) = (pixels, stride);
        if self.settings.half_size {
            convert::halve(source, stride, width, height, &mut self.scaled);
            (source, stride) = (&self.scaled, width / 2 * 4);
        }
        let full_chroma = encoder.full_chroma;
        convert::bgrx_to_yuv(source, stride, encoder.width, encoder.height, encoder.planes(), full_chroma);
        self.refinements_left = REFINE_SECONDS * REFINE_FPS;
        self.encode(started, self.settings.fps);
    }

    /// Encodes the latest picture once more if the screen has stood still long enough and the picture is not
    /// yet as good as it should be.
    pub fn refine(&mut self) {
        if self.refinements_left > 0 && self.encoded.elapsed() >= REFINE_INTERVAL {
            self.refinements_left -= 1;
            self.encode(Instant::now(), REFINE_FPS);
        }
    }

    /// Opens the encoder for a screen of this size and sends the stream's header and decoder configuration.
    fn open(&mut self, width: usize, height: usize) -> Result<(), String> {
        self.screen = (width, height);
        let scale = if self.settings.half_size { 2 } else { 1 };
        // 4:2:0 needs even dimensions; an odd last row or column is dropped.
        let (width, height) = ((width / scale) & !1, (height / scale) & !1);
        let mut encoder =
            Encoder::open(width, height, self.settings.fps, self.settings.kbps, self.settings.full_chroma)
                .ok_or("could not open the encoder")?;
        self.rate = (self.settings.kbps, self.settings.fps);
        eprintln!("remote-desktop: encoding {width}x{height} {}", if encoder.gpu { "on the GPU" } else { "with x264" });
        if !self.wrote_header {
            self.send(b"FLV\x01\x01\x00\x00\x00\x09\x00\x00\x00\x00");
            self.wrote_header = true;
        }
        let configuration = encoder.configuration();
        self.send_tag(true, 0, &configuration);
        self.encoder = Some(encoder);
        Ok(())
    }

    /// Encodes the picture currently in the encoder's planes, as one of `fps` frames a second, and sends it.
    fn encode(&mut self, started: Instant, fps: u32) {
        let Some(encoder) = &mut self.encoder else { return };
        // The session may allow another bitrate by now, and refining goes at another pace than the screen.
        let wanted = match self.shared.kbps.load(Ordering::Relaxed) {
            0 => self.settings.kbps,
            kbps => kbps,
        };
        if self.rate != (wanted, fps) && encoder.set_rate(wanted, fps) {
            self.rate = (wanted, fps);
            self.settings.kbps = wanted;
        }
        if let Some(encoded) = encoder.encode() {
            let (unit, keyframe, settled) = (encoded.unit.to_vec(), encoded.keyframe, encoded.settled);
            self.shared.encode_micros.store(started.elapsed().as_micros() as u32, Ordering::Relaxed);
            self.fine_frames = if settled { self.fine_frames + 1 } else { 0 };
            if self.fine_frames >= SETTLED_FRAMES {
                self.refinements_left = 0;
            }
            self.encoded = Instant::now();
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
