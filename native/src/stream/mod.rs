//! Turns pictures of the screen into the video stream: each one is encoded, a still one is refined, and the
//! result is wrapped in FLV video tags for the sink. Where the pictures come from is the capture's business.

use std::ffi::c_int;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

mod convert;
mod encode;
mod flv;
mod record;

pub use encode::{GpuBuffer, PROBE, probe};
pub use flv::resume;

use crate::capture::{self, HEIGHTS, Output, Settings, Shape, Shared, Sink};
use convert::Shrinker;
use encode::{Codec, EFFORTS, Encoder, LONGEST_BUFFER, MOVING_QUALITY, STILL_QUALITY, Setup, Want};
use flv::Packet;

/// How many times a second a still picture is encoded again to bring it up to the quality target. Far fewer
/// than the frame rate, so that each of these frames may take a correspondingly larger share of the bitrate cap:
/// with a frame's ordinary share an encoder soon finds nothing it can add.
pub const REFINE_FPS: u32 = 10;
/// For how many seconds at most a still picture is refined.
const REFINE_SECONDS: u32 = 20;
/// The longest the capture waits for the screen to change before it looks whether it is still wanted.
const LONGEST_WAIT: Duration = Duration::from_millis(250);
const REFINE_INTERVAL: Duration = Duration::from_millis(1000 / REFINE_FPS as u64);
/// For how many milliseconds of the bitrate cap one frame may take what it allows (see `encode::LONGEST_BUFFER`).
/// A refinement has nothing close behind it and may take the longest. A new picture's frame has others close
/// behind it, which wait while it leaves: it may take half the client's patience, so that on a link that
/// carries little more than the rate they do not wait for longer than that, and never less than two frames'
/// worth.
fn moving_buffer(patience: u32, fps: u32) -> u32 {
    (patience / 2).clamp(2000 / fps, LONGEST_BUFFER)
}

/// Tells when refining a still picture has stopped paying. An encoder goes on spending a little on a picture
/// that does not change, and how much depends on its settings, the picture's size and the machine's cores, so
/// no size as such says "nothing changed". What does is that the frames have stopped getting smaller, at a
/// size far below what the bitrate lets a frame be: near that, they are still filling in detail.
#[derive(Default)]
pub struct Still {
    smallest: Option<usize>,
    level_frames: u32,
}

impl Still {
    /// How many frames in a row have to be level before the picture counts as finished,
    const LEVEL_FRAMES: u32 = 3;
    /// a frame being level when it has at least this many tenths of the smallest frame's size,
    const LEVEL_TENTHS: usize = 9;
    /// and far below what the bitrate allows when under this fraction of it.
    const FAR_BELOW: usize = 4;

    /// The share of a bitrate in kbit/s that one refinement may take, in bytes.
    pub fn share(kbps: u32) -> usize {
        kbps as usize * 125 / REFINE_FPS as usize
    }

    /// Takes the size of the picture's latest frame, the share it could have taken and whether the cap's budget
    /// held it back; says whether the picture is finished. A frame held back says nothing of that: at a coarser
    /// quantiser than the picture already has an encoder adds nothing, and so sends small frames long before the
    /// picture is as good as it should be.
    pub fn finished(&mut self, size: usize, share: usize, held_back: bool) -> bool {
        if held_back {
            self.level_frames = 0;
            return false;
        }
        let level = self.smallest.is_some_and(|smallest| size * 10 >= smallest * Self::LEVEL_TENTHS);
        self.smallest = Some(self.smallest.map_or(size, |smallest| smallest.min(size)));
        self.level_frames = if level && size * Self::FAR_BELOW < share { self.level_frames + 1 } else { 0 };
        self.level_frames >= Self::LEVEL_FRAMES
    }
}

/// The software encoder can put more work into a frame, which makes it look better at the same size. It is
/// asked to where that shows and fits: when the bitrate cap, not the quality target, decided how good most
/// of the last `EFFORT_FRAMES` new frames were, and a frame would still be done in half the time until the
/// next if it took `STEP_COST` times as long. Frames made while the bitrate is still climbing do not count:
/// they are coarse on any link. When frames take longer than that half, it goes a step back and does not try
/// again; so a screen too large for the first effort is soon given the least.
const EFFORT_FRAMES: u32 = 60;

/// More frames a second than this are kept only where they fit (see `keeps_pace`): the rate otherwise falls back
/// to it.
const STEADY_FPS: u32 = 60;
const STEP_COST: u32 = 2;
/// The effort a stream begins at: superfast, whose fewer bits are worth its little more time where that fits.
const FIRST_EFFORT: u32 = 1;

/// Full colour detail (4:4:4) keeps coloured text and thin lines crisp, for twice the samples of 4:2:0 to
/// convert, encode, send and decode; browsers decode it in software only, so the client's time counts as well.
/// Where the client leaves the choice to the host, the stream begins with it and weighs it with the effort: it
/// gives full colour up first, when new frames take longer than the budget or the bitrate cap decided most of
/// them, since a coarse picture loses more than colour detail does. It tries again when frames are done in a
/// `FULL_CHROMA_COST`th of the budget and fine as they are, after a `Rest`.
const FULL_CHROMA_COST: u32 = 2;

/// Where the client leaves the picture's size to the host, a settled bitrate cap at which most new
/// frames come out poor (see encoder.h) makes the picture a step smaller (see `capture::HEIGHTS`): fewer pixels,
/// each given more of the bits. Merely coarse frames at the screen's own size show text better than fine ones
/// of a smaller picture. After a `Rest`, motion that encodes well can gain a step. A still picture
/// that has finished refining can go straight back to native detail.
///
/// The rest before something given up is tried again, which doubles each time it has to be given up anew up to
/// the limit, so that a link it does not suit is not put through a keyframe again and again.
const REST: Duration = Duration::from_secs(30);
const REST_LIMIT: Duration = Duration::from_secs(600);
/// How long a reduced desktop must stay still before restoring the screen's native text detail.
const RESTORE_NATIVE_AFTER: Duration = Duration::from_secs(2);

/// A rest before trying again what had to be given up (see `REST`).
struct Rest {
    until: Instant,
    next: Duration,
}

impl Rest {
    fn new(now: Instant) -> Self {
        Self { until: now, next: REST }
    }

    fn over(&self, now: Instant) -> bool {
        now >= self.until
    }

    /// Something was given up: the next try waits, longer than the last time.
    fn begin(&mut self, now: Instant) {
        self.until = now + self.next;
        self.next = (self.next * 2).min(REST_LIMIT);
    }
}

/// The next height for a moving picture. Step down when most frames are poor at a settled bitrate,
/// and up after a rest when they are fine.
fn next_height(height: Option<usize>, screen_height: usize, poor: bool, fine: bool, rested: bool) -> Option<usize> {
    let steps = HEIGHTS.into_iter().filter(|&step| step < screen_height);
    let now = height.unwrap_or(screen_height);
    if poor {
        steps.clone().find(|&step| step < now).or(height)
    } else if fine && rested && height.is_some() {
        steps.rev().find(|&step| step > now)
    } else {
        height
    }
}

/// Whether to go on with full colour detail.
fn next_chroma(full: bool, average: Duration, budget: Duration, mostly_coarse: bool, rested: bool) -> bool {
    if full {
        average <= budget && !mostly_coarse
    } else {
        rested && !mostly_coarse && average * FULL_CHROMA_COST < budget
    }
}

/// What the new frames since the encoder's effort was last looked at added up to.
#[derive(Default)]
struct Weighed {
    frames: u32,
    coarse: u32,
    poor: u32,
    time: Duration,
    /// Of the gaps between those frames, how many came at the wanted rate, and how many at `STEADY_FPS`.
    quick: u32,
    steady: u32,
}

/// Whether more frames a second than `STEADY_FPS` keep their pace: each is encoded within its interval, and the
/// screen gives them that often. A screen that refreshes at the steady rate showed many gaps of that length
/// among the last frames (`steady`) and none shorter (`quick`); frames merely far apart, as typing makes them,
/// say nothing.
fn keeps_pace(fps: u32, average: Duration, quick: u32, steady: u32) -> bool {
    fps <= STEADY_FPS || (average <= Duration::from_secs(1) / fps && (quick > 0 || steady < EFFORT_FRAMES / 2))
}

/// The effort to go on with, and the most that may be tried from then on.
fn next_effort(effort: u32, most: u32, average: Duration, budget: Duration, mostly_coarse: bool) -> (u32, u32) {
    if average > budget && effort > 0 {
        (effort - 1, effort - 1)
    } else if mostly_coarse && effort < most && average * STEP_COST < budget {
        (effort + 1, most)
    } else {
        (effort, most)
    }
}

/// What a frame is: a new picture of the screen, or the latest one encoded again to refine it. A client that
/// needs a frame to begin with is given the latest picture as a new one: at a moving picture's quality it is
/// smaller, and so there sooner, and refining makes it sharp from there.
#[derive(Clone, Copy, PartialEq)]
enum Frame {
    New,
    Refined,
}

pub struct Stream {
    pub settings: Settings,
    shared: Arc<Shared>,
    sink: Sink,
    /// The size of the screen the encoder is open for.
    screen: (usize, usize),
    /// What shrinks the screen to the picture, and the shrunk copy, when the picture is smaller than the
    /// screen (see `Settings::height`).
    shrinker: Option<Shrinker>,
    /// A change of size, colour or effort takes effect on the next source picture, which a still screen's
    /// capture is `asked` for once.
    reopen: bool,
    asked: bool,
    /// The height that poor motion had come down to before a still picture went back to the screen's own.
    settled: Option<usize>,
    scaled: Vec<u8>,
    encoder: Option<Encoder>,
    /// How much work the software encoder puts into a frame and the most it may (see `EFFORT_FRAMES`).
    effort: u32,
    most_effort: u32,
    weighed: Weighed,
    /// When capture supplied the last source picture. Refinement and recovery frames do not reset this clock.
    last_picture: Option<Instant>,
    /// With colour detail left to the stream: when full colour may be tried again, and the rest after the
    /// next time it has to be given up.
    chroma_rest: Rest,
    size_rest: Rest,
    /// Set once the GPU's encoder, which takes no 4:4:4, was chosen over full colour detail.
    full_chroma_unavailable: bool,
    /// What the encoder that is open encodes.
    codec: Codec,
    /// The bitrate cap in kbit/s, the frames a second, the quality target and the buffer in milliseconds that
    /// the encoder was last told.
    rate: (u32, u32, u32, u32),
    /// A bitrate cap can force a frame out coarser than wanted. The screen may then stay still, and no further
    /// frame would ever improve it, so the same picture is encoded again until that stops paying.
    refinements_left: u32,
    still: Still,
    /// When the latest frame, new or refined, was encoded, and the id the next one gets (see `capture::Frame`).
    encoded: Instant,
    next_id: u64,
    /// How many encoders were opened, which tells a buffer on the GPU of an encoder gone (see `gpu_picture`).
    opened: u64,
    started: Instant,
    wrote_header: bool,
    /// The pointer's shape as the session last heard of it.
    pointer: Option<Shape>,
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
            shrinker: None,
            reopen: false,
            asked: false,
            settled: None,
            scaled: Vec::new(),
            encoder: None,
            effort: FIRST_EFFORT,
            most_effort: EFFORTS - 1,
            weighed: Weighed::default(),
            last_picture: None,
            chroma_rest: Rest::new(now),
            size_rest: Rest::new(now),
            full_chroma_unavailable: false,
            codec: Codec::H264,
            rate: (0, 0, 0, 0),
            refinements_left: 0,
            still: Still::default(),
            encoded: now,
            next_id: 0,
            opened: 0,
            started: now,
            wrote_header: false,
            pointer: None,
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

    #[cfg(windows)]
    pub fn same_display(&self, screen: &crate::screen::Screen) -> Result<bool, String> {
        let current = self.shared.screens.refresh()?;
        Ok(crate::screen::choose(&current, self.settings.screen.as_deref()) == Some(screen))
    }

    /// How long the capture may wait for the screen to change before it calls `refine` and looks at `wanted`
    /// again: a still screen sends nothing, so not long, and no longer than until the next refinement is due.
    pub fn patience(&self) -> Duration {
        let refinement = if self.refinements_left > 0 {
            REFINE_INTERVAL.saturating_sub(self.encoded.elapsed())
        } else {
            Duration::MAX
        };
        refinement.min(LONGEST_WAIT)
    }

    /// Encodes a new picture of the screen. `pixels` holds its rows, `stride` bytes apart, each pixel as blue,
    /// green, red and a fourth byte that is ignored.
    pub fn picture(&mut self, pixels: &[u8], stride: usize, width: usize, height: usize) {
        let started = Instant::now();
        if (self.encoder.is_none() || self.screen != (width, height) || self.reopen)
            && let Err(reason) = self.open(width, height)
        {
            self.ended = Some(Err(reason));
            return;
        }
        let Some(encoder) = &mut self.encoder else { return };
        let (mut source, mut stride) = (pixels, stride);
        if let Some(shrinker) = &mut self.shrinker {
            shrinker.shrink(source, stride, &mut self.scaled);
            (source, stride) = (&self.scaled, encoder.width * convert::BYTES_PER_PIXEL);
        }
        let full_chroma = encoder.full_chroma;
        convert::bgrx_to_yuv(source, stride, encoder.width, encoder.height, encoder.planes(), full_chroma);
        self.encode_picture(started);
    }

    /// A buffer on the GPU for a screen of `width` x `height` to be copied into, where the encoder takes its
    /// pictures that way, and which encoder it belongs to (see `gpu_picture`). None where it does not: the
    /// capture then hands over pictures in memory (`picture`).
    pub fn gpu_buffer(&mut self, width: usize, height: usize) -> Option<(GpuBuffer, u64)> {
        if (self.encoder.is_none() || self.screen != (width, height) || self.reopen)
            && let Err(reason) = self.open(width, height)
        {
            self.ended = Some(Err(reason));
            return None;
        }
        let buffer = self.encoder.as_mut()?.gpu_buffer(width, height)?;
        Some((buffer, self.opened))
    }

    /// Encodes the new picture of the screen in the buffer on the GPU of encoder `opened`, made at its size
    /// there. False, with nothing encoded, where that encoder is gone or another size is due: the capture
    /// then asks for a new buffer.
    pub fn gpu_picture(&mut self, opened: u64) -> bool {
        let started = Instant::now();
        if opened != self.opened || self.reopen || !self.encoder.as_mut().is_some_and(Encoder::take_gpu_picture) {
            return false;
        }
        self.encode_picture(started);
        true
    }

    fn encode_picture(&mut self, started: Instant) {
        let gap = self.last_picture.replace(started).map(|previous| started - previous);
        self.refine_anew();
        if let Some((took, coarse, poor)) = self.encode(started, Frame::New)
            && !self.shared.climbing.load(Ordering::Relaxed)
        {
            self.weigh_effort(gap, took, coarse, poor);
        }
    }

    /// Counts a new frame and, every `EFFORT_FRAMES` of them, reconsiders the colour detail where it is the
    /// stream's to choose, and else the software encoder's effort.
    fn weigh_effort(&mut self, gap: Option<Duration>, took: Duration, coarse: bool, poor: bool) {
        let now = Instant::now();
        let steady = Duration::from_secs(1) / STEADY_FPS;
        self.weighed.quick += gap.is_some_and(|gap| gap < steady * 3 / 4) as u32;
        self.weighed.steady += gap.is_some_and(|gap| gap >= steady * 3 / 4 && gap < steady * 5 / 4) as u32;
        self.weighed.frames += 1;
        self.weighed.coarse += coarse as u32;
        self.weighed.poor += poor as u32;
        self.weighed.time += took;
        if self.weighed.frames < EFFORT_FRAMES {
            return;
        }
        let weighed = std::mem::take(&mut self.weighed);
        let (average, budget) = (weighed.time / weighed.frames, self.frame_interval() / 2);
        let mostly_coarse = weighed.coarse * 2 > weighed.frames;
        if !keeps_pace(self.settings.fps, average, weighed.quick, weighed.steady) {
            warn!("{} frames a second do not fit here: {STEADY_FPS} from now on", self.settings.fps);
            self.settings.fps = STEADY_FPS;
            return;
        }
        if self.settings.auto_chroma && !self.full_chroma_unavailable {
            // A frame is late if either end is slow with it, and full colour detail costs the decoder as well.
            let decode = Duration::from_micros(self.shared.decode_micros.load(Ordering::Relaxed).into());
            let full = self.settings.full_chroma;
            let wanted = next_chroma(full, average.max(decode), budget, mostly_coarse, self.chroma_rest.over(now));
            if wanted != full {
                if full {
                    self.chroma_rest.begin(now);
                }
                self.settings.full_chroma = wanted;
                self.reopen = true;
                return;
            }
        }
        if self.settings.auto_size {
            let poor = weighed.poor * 2 > weighed.frames; // measured after the bitrate settled
            let height = self.settings.height;
            // Motion that is poor again after a still picture goes straight back to the size it had: one
            // keyframe, not one for every step, and no longer rest, as nothing new was given up.
            let back = self.settled.take_if(|_| poor);
            let wanted =
                back.or_else(|| next_height(height, self.screen.1, poor, !mostly_coarse, self.size_rest.over(now)));
            if wanted != height {
                if back.is_none() && wanted.unwrap_or(usize::MAX) < height.unwrap_or(usize::MAX) {
                    self.size_rest.begin(now);
                }
                self.settings.height = wanted;
                self.reopen = true;
                return;
            }
        }
        if self.encoder.as_ref().is_none_or(|encoder| encoder.gpu) {
            return;
        }
        let (effort, most) = next_effort(self.effort, self.most_effort, average, budget, mostly_coarse);
        if effort != self.effort {
            self.reopen = true;
        }
        (self.effort, self.most_effort) = (effort, most);
    }

    /// Starts refining the latest picture from the beginning: it is new, or a client begins with it.
    fn refine_anew(&mut self) {
        (self.refinements_left, self.still) = (REFINE_SECONDS * REFINE_FPS, Still::default());
    }

    /// Refines the current picture while idle. Returns true once if capture must supply native pixels
    /// to apply new encoder settings and the desktop has stopped producing frames on its own. Until it
    /// does, the encoder that is open goes on serving a client that needs a frame.
    pub fn refine(&mut self) -> bool {
        if self.encoder.is_none() {
            return false;
        }
        let idle = self.encoded.elapsed() >= REFINE_INTERVAL;
        if idle
            && self.settings.auto_size
            && self.settings.height.is_some_and(|height| height < self.screen.1)
            && self.still.level_frames >= Still::LEVEL_FRAMES
            && self.last_picture.is_some_and(|source| source.elapsed() >= RESTORE_NATIVE_AFTER)
        {
            // A stationary desktop can spend the whole refinement budget on a single native image.
            self.settled = self.settings.height.take();
            self.reopen = true;
        }
        if self.reopen && idle && !self.asked {
            self.asked = true;
            return true;
        }
        if self.shared.fresh.load(Ordering::Relaxed) || self.shared.recover.load(Ordering::Relaxed) {
            self.refine_anew();
            self.encode(Instant::now(), Frame::New);
        } else if self.refinements_left > 0 && idle {
            self.refinements_left -= 1;
            self.encode(Instant::now(), Frame::Refined);
        }
        false
    }

    /// Opens the encoder for a screen of this size and sends the stream's header and decoder configuration.
    fn open(&mut self, width: usize, height: usize) -> Result<(), String> {
        // Release the old hardware session before asking for a new one; some encoders admit just one.
        self.encoder = None;
        if self.screen != (width, height) {
            self.screen = (width, height);
            self.put_out(Output::ScreenSize(width, height));
        }
        (self.reopen, self.asked) = (false, false);
        let (width, height) = match self.settings.height {
            Some(most) if height > most => (width * most / height, most),
            _ => (width, height),
        };
        // 4:2:0 needs even dimensions; an odd last row or column is dropped.
        let (width, height) = (width & !1, height & !1);
        let shrinking = self.settings.height.is_some_and(|most| self.screen.1 > most);
        self.shrinker = if shrinking { Some(Shrinker::new(self.screen, (width, height))?) } else { None };
        let (fps, kbps, full_chroma) = (self.settings.fps, self.settings.kbps, self.settings.full_chroma);
        let buffer = moving_buffer(self.shared.patience.load(Ordering::Relaxed), fps);
        let setup = Setup {
            hevc: self.settings.hevc as c_int,
            buffer: buffer as c_int,
            ..Setup::new(width, height, fps, kbps, full_chroma, self.effort)
        };
        let mut encoder = Encoder::open(setup, self.settings.auto_chroma).ok_or("could not open the encoder")?;
        self.opened += 1;
        self.codec = encoder.codec;
        if full_chroma && !encoder.full_chroma {
            // The GPU's encoder took the picture at 4:2:0: full colour detail is not tried again with it.
            (self.settings.full_chroma, self.full_chroma_unavailable) = (false, true);
        }
        self.rate = (kbps, fps, MOVING_QUALITY, buffer);
        let how = match (encoder.gpu, self.effort) {
            (true, _) => format!("as {} on the GPU with {}", encoder.codec.name(), encoder.name),
            (false, effort) => format!("with x264, effort {effort} of {}", EFFORTS - 1),
        };
        let colour = if encoder.full_chroma { "4:4:4" } else { "4:2:0" };
        warn!("encoding {width}x{height} {colour} {how}");
        if !self.wrote_header {
            self.put_out(Output::Video(flv::HEADER.to_vec(), None));
            self.wrote_header = true;
        }
        let configuration = encoder.configuration();
        self.send_tag(Packet::Configuration, &configuration, None);
        self.encoder = Some(encoder);
        Ok(())
    }

    /// Encodes the picture currently in the encoder's planes, as one of `fps` frames a second, and sends it.
    /// Says how long that took since `started`, sending apart, and whether the frame came out coarse, and poor.
    fn encode(&mut self, started: Instant, frame: Frame) -> Option<(Duration, bool, bool)> {
        let confirmed = self.shared.confirmed.load(Ordering::Relaxed).checked_sub(1);
        let want = if self.shared.fresh.swap(false, Ordering::Relaxed) {
            Want::Keyframe
        } else if self.shared.recover.swap(false, Ordering::Relaxed) {
            if confirmed.is_some() { Want::Recover } else { Want::Keyframe }
        } else {
            Want::Next
        };
        let encoder = self.encoder.as_mut()?;
        // The session may allow another bitrate by now, and refining goes at another pace and quality.
        let wanted = match self.shared.kbps.load(Ordering::Relaxed) {
            0 => self.settings.kbps,
            kbps => kbps,
        };
        let (fps, quality, buffer) = match frame {
            Frame::New => {
                let fps = self.settings.fps;
                (fps, MOVING_QUALITY, moving_buffer(self.shared.patience.load(Ordering::Relaxed), fps))
            }
            Frame::Refined => (REFINE_FPS, STILL_QUALITY, LONGEST_BUFFER),
        };
        if self.rate != (wanted, fps, quality, buffer) && encoder.set_rate(wanted, fps, quality, buffer) {
            self.rate = (wanted, fps, quality, buffer);
            self.settings.kbps = wanted;
        }
        let id = self.next_id;
        self.next_id += 1;
        let encoded = encoder.encode(id, want, confirmed)?;
        let (unit, keyframe, coarse, held_back, poor, recovered) = (
            encoded.unit.to_vec(),
            encoded.keyframe,
            encoded.coarse,
            encoded.held_back,
            encoded.poor,
            encoded.recovered,
        );
        // A client that lost its place in the stream is told again how to decode, and begins anew with this frame,
        // unless the frame goes on from one it has.
        if want != Want::Next && !recovered {
            let configuration = encoder.configuration();
            self.send_tag(Packet::Configuration, &configuration, None);
        }
        let took = started.elapsed();
        self.shared.encode_micros.store(took.as_micros() as u32, Ordering::Relaxed);
        // Only a new picture says whether the bitrate holds the video back: a refined one aims higher than that.
        if frame == Frame::New {
            self.shared.coarse.fetch_add(coarse as u32, Ordering::Relaxed);
        }
        if self.still.finished(unit.len(), Still::share(self.rate.0), held_back) {
            self.refinements_left = 0;
        }
        self.encoded = Instant::now();
        let after = if recovered { confirmed } else { None };
        self.send_tag(Packet::Frame { keyframe }, &unit, Some(capture::Frame { id, after }));
        Some((took, coarse, poor))
    }

    /// Sends one video tag, timed from the stream's start, and the frame it carries, if it carries one.
    fn send_tag(&mut self, packet: Packet, payload: &[u8], frame: Option<capture::Frame>) {
        let tag = flv::tag(packet, self.codec, self.started.elapsed().as_millis() as u32, payload);
        self.put_out(Output::Video(tag, frame));
    }

    /// Passes the pointer's shape on, unless it is the one passed on last.
    pub fn pointer(&mut self, shape: Shape) {
        if self.pointer.as_ref() != Some(&shape) {
            self.put_out(Output::Pointer(&shape));
            self.pointer = Some(shape);
        }
    }

    #[cfg(windows)]
    pub fn pointer_visible(&mut self, visible: bool) {
        self.put_out(Output::PointerVisible(visible));
    }

    fn put_out(&mut self, output: Output) {
        if !(self.sink)(output) {
            self.ended.get_or_insert(Ok(()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_still_picture_is_finished_when_its_frames_stop_shrinking() {
        let share = Still::share(3000);
        let finished_after = |sizes: &[usize]| {
            let mut still = Still::default();
            sizes.iter().position(|&size| still.finished(size, share, false)).map(|index| index + 1)
        };
        assert_eq!(finished_after(&[60000, 30000, 9000, 900, 200, 150, 150, 150, 150]), Some(9), "at its floor");
        // x264 at its highest effort goes on spending some kilobytes a frame, in a cycle.
        assert_eq!(finished_after(&[60000, 20000, 4000, 2962, 2925, 3091, 2962]), Some(7));
        assert_eq!(finished_after(&[30000; 50]), None, "frames at the limit are still filling in detail");
        assert_eq!(finished_after(&[9000, 5000, 3000, 1800, 1000, 600]), None, "frames getting smaller are too");
        let mut still = Still::default();
        let held_back = (0..5).any(|_| still.finished(20, share, true));
        assert!(!held_back, "tiny frames the cap held back do not finish the picture");
    }

    #[test]
    fn more_frames_than_the_steady_rate_are_kept_only_where_they_fit() {
        let (quick, slow) = (Duration::from_millis(6), Duration::from_millis(11));
        assert!(keeps_pace(60, slow * 2, 0, 59), "the steady rate always");
        assert!(keeps_pace(120, quick, 40, 10), "frames that came quickly and were encoded in time");
        assert!(!keeps_pace(120, slow, 40, 10), "encoded too slowly");
        assert!(!keeps_pace(120, quick, 0, 50), "a screen that refreshes 60 times a second");
        assert!(keeps_pace(120, quick, 0, 5), "frames far apart say nothing");
    }

    #[test]
    fn effort_goes_up_where_it_shows_and_fits_and_down_for_good() {
        let (budget, quick, slow) = (Duration::from_millis(8), Duration::from_millis(3), Duration::from_millis(9));
        assert_eq!(next_effort(0, 2, quick, budget, true), (1, 2), "coarse frames and time to spare");
        assert_eq!(next_effort(0, 2, quick, budget, false), (0, 2), "good frames need no more work");
        assert_eq!(next_effort(0, 2, Duration::from_millis(5), budget, true), (0, 2), "twice as long would not fit");
        assert_eq!(next_effort(2, 2, quick, budget, true), (2, 2), "there is no more");
        assert_eq!(next_effort(1, 2, slow, budget, true), (0, 0), "too slow: back, and not tried again");
        assert_eq!(next_effort(0, 0, quick, budget, true), (0, 0));
        assert_eq!(next_effort(0, 2, slow, budget, true), (0, 2), "nothing below the least");
    }

    #[test]
    fn full_colour_detail_stays_where_it_fits_and_returns_after_a_rest() {
        let (budget, quick, middling, slow) =
            (Duration::from_millis(8), Duration::from_millis(3), Duration::from_millis(5), Duration::from_millis(9));
        assert!(next_chroma(true, middling, budget, false, false), "fits: kept");
        assert!(!next_chroma(true, slow, budget, false, true), "over the budget: given up");
        assert!(!next_chroma(true, quick, budget, true, true), "the bitrate decides: given up");
        assert!(next_chroma(false, quick, budget, false, true), "time to spare after the rest: again");
        assert!(!next_chroma(false, quick, budget, false, false), "not before the rest is over");
        assert!(!next_chroma(false, middling, budget, false, true), "twice the work would not fit");
        assert!(!next_chroma(false, quick, budget, true, true), "coarse frames want the bits elsewhere");
    }

    #[test]
    fn fixed_cap_shrinks_poor_motion_and_restores_native_idle_detail_once() {
        let settings = Settings {
            kbps: 2000,
            fps: 60,
            cursor: false,
            full_chroma: false,
            auto_chroma: false,
            height: None,
            auto_size: true,
            screen: None,
            hevc: false,
        };
        let (sizes, observed) = std::sync::mpsc::channel();
        let mut stream = Stream::new(
            settings,
        let poor_motion = |stream: &mut Stream| {
            stream.weighed = Weighed::default();
            for _ in 0..EFFORT_FRAMES {
                stream.weigh_effort(Some(stream.frame_interval()), Duration::from_millis(1), true, true);
            }
        };
            Arc::new(Shared::default()),
            Box::new(move |output| {
                if let Output::ScreenSize(width, height) = output {
                    sizes.send((width, height)).unwrap();
                }
                true
            }),
        );
        let (width, height) = (128, 720);
        let pixels = vec![90; width * height * 4];
        let picture = |stream: &mut Stream| stream.picture(&pixels, width * 4, width, height);
        picture(&mut stream);
        poor_motion(&mut stream);
        assert_eq!(stream.settings.height, Some(540), "poor frames at the user's cap");
        picture(&mut stream);
        poor_motion(&mut stream);
        picture(&mut stream);
        assert_eq!(stream.encoder.as_ref().unwrap().height, 360, "a step at a time");

        // Quiet alone is not enough while the encoder is still adding detail.
        stream.last_picture = Some(Instant::now() - RESTORE_NATIVE_AFTER);
        stream.encoded = Instant::now() - REFINE_INTERVAL;
        assert!(!stream.refine());
        picture(&mut stream);
        // Let the ordinary still-picture encoder settle without waiting for the motion cooldown.
        while stream.refinements_left > 0 {
            stream.encoded = Instant::now() - REFINE_INTERVAL;
            assert!(!stream.refine());
        }

        let rest = stream.size_rest.until;
        poor_motion(&mut stream);
        assert_eq!(stream.settings.height, Some(360), "poor motion again: straight back to the size it had");
        assert_eq!(stream.size_rest.until, rest, "which is no reason for a longer rest");
        assert!(stream.still.level_frames >= Still::LEVEL_FRAMES);
        stream.encoded = Instant::now() - REFINE_INTERVAL;
        assert!(!stream.refine(), "a short pause keeps the current size");
        stream.last_picture = Some(Instant::now() - RESTORE_NATIVE_AFTER);
        assert!(!stream.size_rest.over(Instant::now()));
        assert!(stream.refine(), "ask capture for native pixels without any desktop changes");
        assert!(!stream.refine(), "only one request until capture supplies those pixels");
        picture(&mut stream);
        assert_eq!(stream.encoder.as_ref().unwrap().height, height);
        assert_eq!(stream.opened, 4, "one encoder per resolution");
        assert_eq!(observed.try_iter().collect::<Vec<_>>(), [(width, height)], "the pointer keeps its native scale");
    }

    #[test]
    fn auto_size_steps_down_on_a_weak_link_and_back_up_after_a_rest() {
        let screen = 1200; // 1080, 720, 540 and 360 are smaller
        let (poor, fine, coarse) = ((true, false), (false, true), (false, false));
        let next = |height, (poor, fine), rested| next_height(height, screen, poor, fine, rested);
        assert_eq!(next(None, poor, false), Some(1080), "the screen's own size, then the largest step below it");
        assert_eq!(next(Some(1080), poor, false), Some(720));
        assert_eq!(next(Some(360), poor, false), Some(360), "nothing below the smallest");
        assert_eq!(next(Some(720), fine, false), Some(720), "not before the rest is over");
        assert_eq!(next(Some(720), fine, true), Some(1080));
        assert_eq!(next(Some(1080), fine, true), None, "back to the screen's own size");
        assert_eq!(next(None, fine, true), None);
        assert_eq!(next(None, coarse, true), None, "coarse but not poor: stays");
        assert_eq!(next(Some(720), coarse, true), Some(720), "and does not grow either");
        assert_eq!(next_height(None, 720, true, false, false), Some(540), "a screen of 720 rows begins below it");
    }
}
