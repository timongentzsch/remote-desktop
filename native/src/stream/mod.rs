//! Turns pictures of the screen into the video stream: each one is encoded, a still one is refined, and the
//! result is wrapped in FLV video tags for the sink. Where the pictures come from is the capture's business.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

mod convert;
mod encode;
mod flv;

pub use encode::{PROBE, probe};

use crate::capture::{Output, Settings, Shape, Shared, Sink};
use convert::Shrinker;
use encode::{EFFORTS, Encoder, MOVING_QUALITY, STILL_QUALITY, Setup};
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

    /// Takes the size of the picture's latest frame and the share it could have taken; says whether the
    /// picture is finished.
    pub fn finished(&mut self, size: usize, share: usize) -> bool {
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
/// again.
const EFFORT_FRAMES: u32 = 60;
const STEP_COST: u32 = 2;

/// Full colour detail (4:4:4) keeps coloured text and thin lines crisp, for twice the samples of 4:2:0 to
/// convert, encode and send. Where the client leaves the choice to the host, the stream begins with it and
/// weighs it with the effort: it gives full colour up first, when new frames take longer than the budget or the
/// bitrate cap decided most of them, since a coarse picture loses more than colour detail does. It tries again
/// when frames are done in a `FULL_CHROMA_COST`th of the budget and fine as they are, after a rest that
/// doubles each time full colour had to be given up, so that a link it does not suit is not put through a
/// keyframe again and again.
const FULL_CHROMA_COST: u32 = 2;
const CHROMA_REST: Duration = Duration::from_secs(30);
const CHROMA_REST_LIMIT: Duration = Duration::from_secs(600);

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
    time: Duration,
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
    scaled: Vec<u8>,
    encoder: Option<Encoder>,
    /// How much work the software encoder puts into a frame and the most it may (see `EFFORT_FRAMES`).
    effort: u32,
    most_effort: u32,
    weighed: Weighed,
    /// With colour detail left to the stream: when full colour may be tried again, and the rest after the
    /// next time it has to be given up.
    chroma_rested_at: Instant,
    chroma_rest: Duration,
    /// The bitrate cap in kbit/s, the frames a second and the quality target the encoder was last told.
    rate: (u32, u32, u32),
    /// A bitrate cap can force a frame out coarser than wanted. The screen may then stay still, and no further
    /// frame would ever improve it, so the same picture is encoded again until that stops paying.
    refinements_left: u32,
    still: Still,
    /// When the latest frame, new or refined, was encoded.
    encoded: Instant,
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
            scaled: Vec::new(),
            encoder: None,
            effort: 0,
            most_effort: EFFORTS - 1,
            weighed: Weighed::default(),
            chroma_rested_at: now,
            chroma_rest: CHROMA_REST,
            rate: (0, 0, 0),
            refinements_left: 0,
            still: Still::default(),
            encoded: now,
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
        if (self.encoder.is_none() || self.screen != (width, height))
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
        self.refine_anew();
        if let Some((took, coarse)) = self.encode(started, Frame::New)
            && !self.shared.climbing.load(Ordering::Relaxed)
        {
            self.weigh_effort(took, coarse);
        }
    }

    /// Counts a new frame and, every `EFFORT_FRAMES` of them, reconsiders the colour detail where it is the
    /// stream's to choose, and else the software encoder's effort.
    fn weigh_effort(&mut self, took: Duration, coarse: bool) {
        self.weighed.frames += 1;
        self.weighed.coarse += coarse as u32;
        self.weighed.time += took;
        if self.weighed.frames < EFFORT_FRAMES {
            return;
        }
        let weighed = std::mem::take(&mut self.weighed);
        let (average, budget) = (weighed.time / weighed.frames, self.frame_interval() / 2);
        let mostly_coarse = weighed.coarse * 2 > weighed.frames;
        if self.settings.auto_chroma {
            let now = Instant::now();
            let full = self.settings.full_chroma;
            let wanted = next_chroma(full, average, budget, mostly_coarse, now >= self.chroma_rested_at);
            if wanted != full {
                if full {
                    self.chroma_rested_at = now + self.chroma_rest;
                    self.chroma_rest = (self.chroma_rest * 2).min(CHROMA_REST_LIMIT);
                }
                self.settings.full_chroma = wanted;
                self.encoder = None; // the next picture opens another, which begins with a keyframe
                return;
            }
        }
        if self.encoder.as_ref().is_none_or(|encoder| encoder.gpu) {
            return;
        }
        let (effort, most) = next_effort(self.effort, self.most_effort, average, budget, mostly_coarse);
        if effort != self.effort {
            self.encoder = None; // the next picture opens another, which begins with a keyframe
        }
        (self.effort, self.most_effort) = (effort, most);
    }

    /// Starts refining the latest picture from the beginning: it is new, or a client begins with it.
    fn refine_anew(&mut self) {
        (self.refinements_left, self.still) = (REFINE_SECONDS * REFINE_FPS, Still::default());
    }

    /// Encodes the latest picture once more if the screen has stood still long enough and the picture is not
    /// yet as good as it should be, or if the client needs a frame to begin with.
    pub fn refine(&mut self) {
        if self.shared.fresh.load(Ordering::Relaxed) && self.encoder.is_some() {
            // A client waits for a frame to begin with: the latest picture again, refined from there.
            self.refine_anew();
            self.encode(Instant::now(), Frame::New);
        } else if self.refinements_left > 0 && self.encoded.elapsed() >= REFINE_INTERVAL {
            self.refinements_left -= 1;
            self.encode(Instant::now(), Frame::Refined);
        }
    }

    /// Opens the encoder for a screen of this size and sends the stream's header and decoder configuration.
    fn open(&mut self, width: usize, height: usize) -> Result<(), String> {
        self.screen = (width, height);
        let (width, height) = match self.settings.height {
            Some(most) if height > most => (width * most / height, most),
            _ => (width, height),
        };
        // 4:2:0 needs even dimensions; an odd last row or column is dropped.
        let (width, height) = (width & !1, height & !1);
        let shrinking = self.settings.height.is_some_and(|most| self.screen.1 > most);
        self.shrinker = if shrinking { Some(Shrinker::new(self.screen, (width, height))?) } else { None };
        let (fps, kbps, full_chroma) = (self.settings.fps, self.settings.kbps, self.settings.full_chroma);
        let mut encoder = Encoder::open(Setup::new(width, height, fps, kbps, full_chroma, self.effort))
            .ok_or("could not open the encoder")?;
        self.rate = (kbps, fps, MOVING_QUALITY);
        let how = match (encoder.gpu, self.effort) {
            (true, _) => "on the GPU".to_owned(),
            (false, 0) => "with x264".to_owned(),
            (false, effort) => format!("with x264, effort {effort} of {}", EFFORTS - 1),
        };
        let colour = if full_chroma { "4:4:4" } else { "4:2:0" };
        warn!("encoding {width}x{height} {colour} {how}");
        if !self.wrote_header {
            self.send(flv::HEADER);
            self.wrote_header = true;
        }
        let configuration = encoder.configuration();
        self.send_tag(Packet::Configuration, &configuration);
        self.encoder = Some(encoder);
        Ok(())
    }

    /// Encodes the picture currently in the encoder's planes, as one of `fps` frames a second, and sends it.
    /// Says how long that took since `started`, sending apart, and whether the frame came out coarse.
    fn encode(&mut self, started: Instant, frame: Frame) -> Option<(Duration, bool)> {
        // A client that lost its place in the stream is told again how to decode, and begins anew with this frame.
        let fresh = self.shared.fresh.swap(false, Ordering::Relaxed);
        if fresh {
            let configuration = self.encoder.as_mut()?.configuration();
            self.send_tag(Packet::Configuration, &configuration);
        }
        let encoder = self.encoder.as_mut()?;
        // The session may allow another bitrate by now, and refining goes at another pace and quality.
        let wanted = match self.shared.kbps.load(Ordering::Relaxed) {
            0 => self.settings.kbps,
            kbps => kbps,
        };
        let (fps, quality) = match frame {
            Frame::New => (self.settings.fps, MOVING_QUALITY),
            Frame::Refined => (REFINE_FPS, STILL_QUALITY),
        };
        if self.rate != (wanted, fps, quality) && encoder.set_rate(wanted, fps, quality) {
            self.rate = (wanted, fps, quality);
            self.settings.kbps = wanted;
        }
        let encoded = encoder.encode(fresh)?;
        let (unit, keyframe, coarse) = (encoded.unit.to_vec(), encoded.keyframe, encoded.coarse);
        let took = started.elapsed();
        self.shared.encode_micros.store(took.as_micros() as u32, Ordering::Relaxed);
        // Only a new picture says whether the bitrate holds the video back: a refined one aims higher than that.
        if frame == Frame::New {
            self.shared.coarse.fetch_add(coarse as u32, Ordering::Relaxed);
        }
        if self.still.finished(unit.len(), Still::share(self.rate.0)) {
            self.refinements_left = 0;
        }
        self.encoded = Instant::now();
        self.send_tag(Packet::Frame { keyframe }, &unit);
        Some((took, coarse))
    }

    /// Sends one video tag, timed from the stream's start.
    fn send_tag(&mut self, packet: Packet, payload: &[u8]) {
        let tag = flv::tag(packet, self.started.elapsed().as_millis() as u32, payload);
        self.send(&tag);
    }

    /// Passes the pointer's shape on, unless it is the one passed on last.
    pub fn pointer(&mut self, shape: Shape) {
        if self.pointer.as_ref() != Some(&shape) {
            self.put_out(Output::Pointer(&shape));
            self.pointer = Some(shape);
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.put_out(Output::Video(bytes));
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
            sizes.iter().position(|&size| still.finished(size, share)).map(|index| index + 1)
        };
        assert_eq!(finished_after(&[60000, 30000, 9000, 900, 200, 150, 150, 150, 150]), Some(9), "at its floor");
        // x264 at its highest effort goes on spending some kilobytes a frame, in a cycle.
        assert_eq!(finished_after(&[60000, 20000, 4000, 2962, 2925, 3091, 2962]), Some(7));
        assert_eq!(finished_after(&[30000; 50]), None, "frames at the limit are still filling in detail");
        assert_eq!(finished_after(&[9000, 5000, 3000, 1800, 1000, 600]), None, "frames getting smaller are too");
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
}
