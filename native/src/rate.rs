//! How much video a client's connection is given. The session's writer reports what it sent and what the
//! client says has arrived; the controller answers with the bitrate for the encoder. It cuts the rate when the
//! link does not take the frames as fast as they are made. When the video could do with more, it finds out
//! whether the link carries more by sending padding for a moment: what was used says nothing about what else
//! there is. A screen that needs no more is not probed for more, which would cost traffic and, on a full
//! link, a gap in the sound.
//!
//! Over TCP only the client's own count and round trip are gone by. What that connection itself knows ends
//! where the connection does, which behind a proxy or in a tunnel is not at the client. Over UDP the
//! connection ends in the client's browser, and its own count of what arrived is the one gone by.

use std::time::{Duration, Instant};

/// The bitrate never goes below this, in kbit/s: less no longer shows a usable desktop.
pub const LOWEST: u32 = 500;
/// The ceiling when the client leaves the choice to the host.
pub const HIGHEST: u32 = 30000;
/// Where a session starts, before the link has shown what it carries. A first picture of this rate arrives
/// quickly on a slow link too.
const START: u32 = 4000;
/// How long the start rate stands before the first probe, so that padding does not hold up the first picture.
const FIRST_REST: Duration = Duration::from_secs(1);

/// Frames are judged over this long at a time.
const WINDOW: Duration = Duration::from_millis(750);
/// The link is too slow for the rate when in that time frames are held up for longer than the client has
/// patience for (see `Reading::patience`), each waiting for the one before it to leave. Frames that are merely
/// large keep the link busy without holding any up. Once it has happened the window is judged at once, if it
/// has lasted long enough to tell what the link delivered.
const SHORTEST_WINDOW: Duration = Duration::from_millis(250);
/// It also lags when the client's round trip is this far above its shortest.
const LATE_LIMIT: Duration = Duration::from_millis(250);
/// A round trip this far above where it was means a queue is building up along the way. A probe ends early
/// then, and none starts.
const QUEUE_LIMIT: Duration = Duration::from_millis(80);
/// And it carries less than it is given when it loses more than this share: something on the way drops what
/// is too much for it, where another would queue it. Loss by chance stays below this on any link worth using.
const HEAVY_LOSS: f64 = 0.1;
/// After lag, the share of what the link delivered that the video may take.
const SHARE: f64 = 0.85;

/// The video could do with more when over the last window it used this share of its rate, or the rate left
/// most of its frames coarser than they should be: at a very low rate an encoder may not even use what it has.
/// Only then is the link probed.
const USED: f64 = 0.7;
/// A probe offers this many times the current rate,
const STEP: f64 = 2.0;
/// for this long plus eight round trips, in which a connection that had gone quiet picks up speed again.
const PROBE_TIME: Duration = Duration::from_millis(600);
const PROBE_LIMIT: Duration = Duration::from_secs(3);
/// The link carries what a probe offered when this share of it arrived,
const CARRIED: f64 = 0.9;
/// and a probe counts as a gain when it raised the rate by this factor.
const GAIN: f64 = 1.1;
/// After a probe that filled the link, what it left under way takes about this long to drain. Frames held up
/// meanwhile say nothing about the rate.
const SETTLING: Duration = Duration::from_millis(1500);
/// The client says this often how much has arrived. Two such counts tell a rate when they are at least
/// `SHORTEST_MEASURE` apart, and no probe starts on a count older than `STALE`, as a hidden page's are.
const REPORT: Duration = Duration::from_millis(100);
const SHORTEST_MEASURE: Duration = Duration::from_millis(150);
const STALE: Duration = Duration::from_millis(500);
/// What a probe has sent and the client has not counted yet is explained by the way there, the wait for the
/// next report and a little under way. More than that, and the link is not taking what it is offered.
const UNDER_WAY: u64 = 32 * 1024;

/// How long the rate rests after lag before a probe. After a probe that gained nothing the rest doubles each
/// time, up to the limit: a full link is not asked again and again.
const REST: Duration = Duration::from_secs(10);
const REST_LIMIT: Duration = Duration::from_secs(60);
/// Lag this soon after a raise is blamed on the raise.
const TRIAL: Duration = Duration::from_secs(5);
/// How long the client's shortest round trip is remembered. A path that became longer for good is then no
/// longer taken for a full one.
const MEMORY: Duration = Duration::from_secs(60);

/// The rate a session starts at under `ceiling`.
pub fn start(ceiling: u32) -> u32 {
    START.min(ceiling)
}

/// How many bytes the client said had arrived, and when it said so.
pub type Mark = (Instant, u64);

/// What the writer knows at one moment.
pub struct Reading {
    pub now: Instant,
    /// Bytes handed to the connection since the session began.
    pub written: u64,
    /// The client's latest count of what has arrived, and the round trip it last measured, once it has
    /// reported them.
    pub received: Option<Mark>,
    pub round_trip: Option<Duration>,
    /// The share of what was sent in the last second that got lost, where the connection itself counts what
    /// arrives, as over UDP. Its count is then exact, and tells what the link carries better than anything.
    pub lost: Option<f64>,
    /// How many frames have left for the client since the last reading, how many the encoder made coarser
    /// than they should be because of the rate,
    pub frames: u32,
    pub coarse: u32,
    /// and for how long the capture could not hand its next frame over because the one before had not left.
    pub held_up: Duration,
    /// The highest bitrate the client wants, in kbit/s, and for how long it would rather have video fall behind
    /// than see the picture made coarser.
    pub ceiling: u32,
    pub patience: Duration,
}

pub struct Verdict {
    /// The bitrate for the encoder, in kbit/s.
    pub kbps: u32,
    /// Bytes of padding to send now.
    pub padding: usize,
    /// Whether a probe is running, in which case the writer asks again within milliseconds.
    pub probing: bool,
    /// Whether the link, not the ceiling, keeps the rate where it is.
    pub held_back: bool,
    /// Whether the rate is neither there nor at the ceiling yet, but still on its way up.
    pub climbing: bool,
}

pub struct Controller {
    kbps: u32,
    ceiling: u32,
    limited: bool,
    /// Whether the video could do with more, going by the last window judged (see `USED`).
    wanting: bool,
    window: Window,
    probe: Option<Probe>,
    /// No probe starts before this.
    resting_until: Instant,
    /// Frames are not judged before this.
    settled_at: Instant,
    /// The rest after the next probe that gains nothing.
    patience: Duration,
    /// When a probe last raised the rate.
    raised: Option<Instant>,
    fastest: Fastest,
}

/// The frames sent since `since`.
struct Window {
    since: Instant,
    /// What had been written by then, and what had arrived, if the client had said.
    written: u64,
    arrived: Option<Mark>,
    frames: u32,
    coarse: u32,
    held_up: Duration,
}

impl Window {
    fn new(reading: &Reading) -> Self {
        Self {
            since: reading.now,
            written: reading.written,
            arrived: reading.received,
            frames: 0,
            coarse: 0,
            held_up: Duration::ZERO,
        }
    }
}

struct Probe {
    /// The rate offered, in kbit/s.
    kbps: u32,
    since: Instant,
    length: Duration,
    /// What had been written and had arrived when it began, and the round trip then.
    written: u64,
    arrived: Mark,
    round_trip: Option<Duration>,
    /// What had arrived when its second half began, and by the last count before its end. Only that half
    /// shows the speed the connection worked up to.
    halfway: Option<Mark>,
    latest: Mark,
}

/// The client's shortest round trip of the last minute or two.
struct Fastest {
    recent: Option<Duration>,
    earlier: Option<Duration>,
    turned: Instant,
}

impl Fastest {
    fn note(&mut self, now: Instant, round_trip: Option<Duration>) {
        if now.duration_since(self.turned) >= MEMORY {
            self.earlier = self.recent.take();
            self.turned = now;
        }
        if let Some(round_trip) = round_trip {
            self.recent = Some(self.recent.map_or(round_trip, |shortest| shortest.min(round_trip)));
        }
    }

    fn get(&self) -> Option<Duration> {
        match (self.recent, self.earlier) {
            (Some(recent), Some(earlier)) => Some(recent.min(earlier)),
            (recent, earlier) => recent.or(earlier),
        }
    }
}

/// Whether `now` exceeds `before` by more than `limit`, where both are known.
fn rose(now: Option<Duration>, before: Option<Duration>, limit: Duration) -> bool {
    matches!((now, before), (Some(now), Some(before)) if now > before + limit)
}

/// The rate in kbit/s at which bytes arrived between two marks, if they are far enough apart to tell.
fn carried(from: Option<Mark>, to: Option<Mark>) -> Option<f64> {
    let ((began, before), (ended, after)) = (from?, to?);
    let time = ended.checked_duration_since(began).filter(|time| *time >= SHORTEST_MEASURE)?;
    Some(after.saturating_sub(before) as f64 * 8.0 / 1000.0 / time.as_secs_f64())
}

impl Controller {
    /// A controller for a connection that has been given `written` bytes so far.
    pub fn new(now: Instant, ceiling: u32, written: u64) -> Self {
        Self {
            kbps: start(ceiling),
            ceiling,
            limited: false,
            wanting: false,
            window: Window { since: now, written, arrived: None, frames: 0, coarse: 0, held_up: Duration::ZERO },
            probe: None,
            resting_until: now + FIRST_REST,
            settled_at: now,
            patience: REST,
            raised: None,
            fastest: Fastest { recent: None, earlier: None, turned: now },
        }
    }

    /// Takes the connection's state and decides. The writer calls this after everything it sends and at least
    /// a few times a second.
    pub fn step(&mut self, reading: &Reading) -> Verdict {
        self.fastest.note(reading.now, reading.round_trip);
        self.window.frames += reading.frames;
        self.window.coarse += reading.coarse;
        self.window.held_up += reading.held_up;
        if reading.ceiling != self.ceiling {
            // The client chose anew: what the link carries is found out afresh.
            (self.ceiling, self.resting_until, self.patience) = (reading.ceiling, reading.now, REST);
        }
        if self.kbps >= reading.ceiling {
            (self.kbps, self.limited, self.probe) = (reading.ceiling, false, None);
        }
        let padding = match self.probe.take() {
            Some(probe) => self.continue_probe(probe, reading),
            None => {
                self.judge_window(reading);
                self.probe = self.begin_probe(reading);
                0
            }
        };
        let below = self.kbps < reading.ceiling;
        Verdict {
            kbps: self.kbps,
            padding,
            probing: self.probe.is_some(),
            held_back: self.limited && below,
            climbing: !self.limited && below,
        }
    }

    /// Half the rate, the most one short measurement may take away.
    fn halved(&self) -> u32 {
        (self.kbps / 2).max(LOWEST)
    }

    /// Once the window is over, or frames were held up for too long in it already, cuts the rate if it lagged.
    fn judge_window(&mut self, reading: &Reading) {
        if reading.now < self.settled_at {
            self.window = Window::new(reading);
            return;
        }
        let lasted = reading.now.duration_since(self.window.since);
        let backed_up = self.window.held_up > reading.patience;
        if lasted < if backed_up { SHORTEST_WINDOW } else { WINDOW } {
            return;
        }
        let window = std::mem::replace(&mut self.window, Window::new(reading));
        let written = (reading.written - window.written) as f64 * 8.0 / 1000.0 / lasted.as_secs_f64();
        self.wanting = written >= USED * self.kbps as f64 || window.coarse * 2 > window.frames;
        // Without frames the round trip says nothing about what the video does to the link.
        let late = window.frames > 0 && rose(reading.round_trip, self.fastest.get(), LATE_LIMIT);
        let dropping = window.frames > 0 && reading.lost.is_some_and(|lost| lost > HEAVY_LOSS);
        if !backed_up && !late && !dropping {
            return;
        }
        // Frames held up means the connection took no more than it did, so what was written is what the link
        // carries. A round trip grown long means a queue further along, and heavy loss that the surplus is
        // dropped there: then only what arrived says what the link carries. The client counts whole messages
        // and may have sent little, so going by its count the rate is at most halved. The connection's own
        // count is gone by wherever there is one: frames that were held up while a queue drained say too little.
        let arrived = carried(window.arrived, reading.received);
        let (link_rate, least) = match arrived {
            Some(arrived) if reading.lost.is_some() => (arrived, LOWEST),
            _ if backed_up => (written, LOWEST),
            _ => (arrived.unwrap_or(0.0), self.halved()),
        };
        self.kbps = ((SHARE * link_rate) as u32).clamp(least, self.kbps);
        self.limited = true;
        if dropping {
            // The loss is that of a second past, and goes on being told until one has passed at the new rate.
            self.settled_at = reading.now + SETTLING;
        }
        let blamed_on_raise = self.raised.is_some_and(|at| reading.now.duration_since(at) < TRIAL);
        self.rest(reading.now, blamed_on_raise);
    }

    /// A probe, if the video could do with more than a rate below the ceiling, the rate has rested, and the
    /// client's reports are fresh and show no queue building up already.
    fn begin_probe(&mut self, reading: &Reading) -> Option<Probe> {
        let arrived = reading.received.filter(|(at, _)| reading.now.saturating_duration_since(*at) < STALE)?;
        let queueing = rose(reading.round_trip, self.fastest.get(), QUEUE_LIMIT);
        if self.kbps >= reading.ceiling || !self.wanting || reading.now < self.resting_until || queueing {
            return None;
        }
        self.wanting = false; // what the probe leads to has to prove too little in its turn before the next
        self.window = Window::new(reading); // from here it shows what the probe does
        Some(Probe {
            kbps: ((self.kbps as f64 * STEP) as u32).min(reading.ceiling),
            since: reading.now,
            length: (PROBE_TIME + 8 * reading.round_trip.unwrap_or_default()).min(PROBE_LIMIT),
            written: reading.written,
            arrived,
            round_trip: reading.round_trip,
            halfway: None,
            latest: arrived,
        })
    }

    /// Carries a probe on: says how much padding is due, or ends it and takes what it showed.
    fn continue_probe(&mut self, mut probe: Probe, reading: &Reading) -> usize {
        let elapsed = reading.now.duration_since(probe.since);
        let pace = probe.kbps as f64 * 125.0; // bytes a second
        let due = (pace * elapsed.min(probe.length).as_secs_f64()) as u64;
        let sent = reading.written - probe.written;
        let arrived = reading.received.unwrap_or(probe.latest);
        // With a queue building up along the way, or more under way than the way explains, the link is full and
        // going on would only add delay. So would going on once the padding holds frames up.
        let explained = (pace * (reading.round_trip.unwrap_or_default() + REPORT + QUEUE_LIMIT).as_secs_f64()) as u64;
        let under_way = sent.saturating_sub(arrived.1.saturating_sub(probe.arrived.1));
        let full = rose(reading.round_trip, probe.round_trip, QUEUE_LIMIT) || under_way > explained + UNDER_WAY;
        let holding_up = self.window.held_up > reading.patience;
        if elapsed < probe.length && !full && !holding_up {
            if probe.halfway.is_none() && arrived.0 >= probe.since + probe.length / 2 {
                probe.halfway = Some(arrived);
            }
            probe.latest = arrived;
            self.probe = Some(probe);
            return due.saturating_sub(sent) as usize;
        }

        // A probe that ran its time is measured in its second half, one cut short in what there was of it.
        let ran_out = !full && !holding_up;
        let arrival_rate = match probe.halfway {
            Some(halfway) if ran_out => carried(Some(halfway), Some(probe.latest)),
            _ => carried(Some(probe.arrived), Some(arrived)),
        };
        let kept_pace = ran_out && arrival_rate.is_some_and(|rate| rate >= CARRIED * probe.kbps as f64);
        // What arrived while the link was full is all it carries, even where that is less than the rate so
        // far, which a still screen may never have put to the test. One short measurement halves it at most.
        // Otherwise the rate does not go down.
        let least = if full { self.halved() } else { self.kbps };
        let shown = arrival_rate.map_or(self.kbps, |rate| ((SHARE * rate) as u32).clamp(least, probe.kbps));
        let reached = if kept_pace { probe.kbps } else { shown };
        let gained = reached as f64 >= self.kbps as f64 * GAIN;
        self.kbps = reached;
        self.limited = !kept_pace;
        self.window = Window::new(reading); // frames that waited behind padding are not judged
        if kept_pace {
            self.resting_until = reading.now; // the link may carry more still: the next probe follows at once
        } else {
            self.settled_at = reading.now + SETTLING;
            self.rest(reading.now, !gained);
        }
        if gained {
            (self.patience, self.raised) = (REST, Some(reading.now));
        }
        0
    }

    /// Puts the next probe off: for the usual time, or for the patience that doubles with each such rest. A
    /// rest already under way is not cut short.
    fn rest(&mut self, now: Instant, longer: bool) {
        self.resting_until = self.resting_until.max(now + if longer { self.patience } else { REST });
        if longer {
            self.patience = (self.patience * 2).min(REST_LIMIT);
        }
        self.raised = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_millis(5);
    const ROUND_TRIP: Duration = Duration::from_millis(20);
    const PATIENCE: Duration = Duration::from_millis(75);

    /// A session on a made-up link that carries `capacity` kbit/s, with a client that reports ten times a
    /// second. A link that `queues` takes everything at once and delivers it late, as a path with a large
    /// buffer or a relay does; otherwise the writer has to wait, as it does when the kernel holds data back.
    struct Session {
        controller: Controller,
        now: Instant,
        capacity: u32,
        queues: bool,
        report: Option<Mark>,
        ceiling: u32,
        /// Whether the screen moves, producing a frame of the allowed size 50 times a second.
        moving: bool,
        round_trip: Duration,
        written: u64,
        delivered: f64,
        /// A frame the capture has made and cannot hand over yet. Meanwhile it makes no other.
        made: Option<u64>,
        /// The frame waiting to be written and the one being written, as bytes left.
        waiting: Option<u64>,
        writing: Option<u64>,
        kbps: u32,
        held_back: bool,
        padding: u64,
        /// How often the rate went down.
        cuts: u32,
        /// The longest that anything queued along the way.
        worst_delay: Duration,
    }

    impl Session {
        fn new(capacity: u32) -> Self {
            let now = Instant::now();
            Self {
                controller: Controller::new(now, HIGHEST, 0),
                now,
                capacity,
                queues: false,
                report: None,
                ceiling: HIGHEST,
                moving: true,
                round_trip: ROUND_TRIP,
                written: 0,
                delivered: 0.0,
                made: None,
                waiting: None,
                writing: None,
                kbps: start(HIGHEST),
                held_back: false,
                padding: 0,
                cuts: 0,
                worst_delay: Duration::ZERO,
            }
        }

        fn run(&mut self, time: Duration) {
            for tick in 0..(time.as_millis() / TICK.as_millis()) {
                self.now += TICK;
                let per_tick = self.capacity as f64 * 125.0 * TICK.as_secs_f64();
                self.delivered = (self.delivered + per_tick).min(self.written as f64);
                let outstanding = self.written - self.delivered as u64;
                // The kernel takes what a round trip carries and a little more, a queueing path anything.
                let in_flight = (self.capacity as f64 * 125.0 * ROUND_TRIP.as_secs_f64()) as u64 + 16 * 1024;
                let mut room = if self.queues { u64::MAX } else { in_flight.saturating_sub(outstanding) };
                if tick % 20 == 0 {
                    self.report = Some((self.now, self.delivered as u64));
                }
                if self.moving && tick % 4 == 0 && self.made.is_none() {
                    self.made = Some(self.kbps as u64 * 125 / 50);
                }
                self.waiting = self.waiting.take().or_else(|| self.made.take());
                let held_up = if self.made.is_some() { TICK } else { Duration::ZERO };
                self.writing = self.writing.take().or_else(|| self.waiting.take());
                let mut frames = 0;
                if let Some(left) = self.writing.take() {
                    let wrote = left.min(room);
                    (self.written, room) = (self.written + wrote, room - wrote);
                    if wrote == left {
                        frames = 1;
                    } else {
                        self.writing = Some(left - wrote);
                    }
                }
                let queued = Duration::from_secs_f64(outstanding as f64 / (self.capacity as f64 * 125.0));
                let delay = if self.queues { queued } else { Duration::ZERO };
                self.worst_delay = self.worst_delay.max(delay);
                let verdict = self.controller.step(&Reading {
                    now: self.now,
                    written: self.written,
                    received: self.report,
                    round_trip: Some(self.round_trip + delay),
                    lost: None,
                    frames,
                    coarse: 0,
                    held_up,
                    ceiling: self.ceiling,
                    patience: PATIENCE,
                });
                let padding = (verdict.padding as u64).min(room);
                self.written += padding;
                self.padding += padding;
                self.cuts += (verdict.kbps < self.kbps) as u32;
                (self.kbps, self.held_back) = (verdict.kbps, verdict.held_back);
            }
        }
    }

    const fn seconds(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    #[test]
    fn a_fast_link_is_given_the_ceiling_within_seconds() {
        let mut session = Session::new(100_000);
        session.run(seconds(6));
        assert_eq!(session.kbps, HIGHEST);
        assert!(!session.held_back);
        assert_eq!(session.cuts, 0);
        let spent = session.padding;
        session.run(seconds(60));
        assert_eq!(session.padding, spent, "nothing is probed once the ceiling is reached");
    }

    #[test]
    fn a_slow_link_gets_a_little_less_than_it_carries_and_is_left_alone() {
        let mut session = Session::new(2000);
        session.run(seconds(5));
        assert!((1400..=1900).contains(&session.kbps), "settled at {}", session.kbps);
        assert!(session.held_back);
        let (cuts, spent) = (session.cuts, session.padding);
        session.run(seconds(600));
        assert!((1400..=1900).contains(&session.kbps), "stayed at {}", session.kbps);
        assert_eq!(session.cuts, cuts, "the rate does not see-saw");
        // Ten minutes at this rate are about 130 MB of video.
        assert!(session.padding - spent < 2_000_000, "probing cost {} bytes", session.padding - spent);
    }

    #[test]
    fn the_rate_follows_the_link_down_and_up_again() {
        let mut session = Session::new(100_000);
        session.run(seconds(8));
        session.capacity = 2000;
        session.run(seconds(3));
        assert!((1000..=1900).contains(&session.kbps), "after the link slowed: {}", session.kbps);
        session.run(seconds(20));
        session.capacity = 100_000;
        session.run(seconds(30));
        assert_eq!(session.kbps, HIGHEST, "after the link recovered");
    }

    #[test]
    fn a_screen_that_needs_no_more_is_not_probed_for_more() {
        let mut session = Session::new(2000);
        session.run(seconds(5));
        let (rate, spent) = (session.kbps, session.padding);
        (session.moving, session.capacity) = (false, 100_000);
        session.run(seconds(120));
        assert_eq!((session.kbps, session.padding), (rate, spent), "a still screen is left alone");
        session.moving = true;
        session.run(seconds(12));
        assert_eq!(session.kbps, HIGHEST, "and gets what the link carries once it moves");
    }

    #[test]
    fn coarse_frames_ask_for_more_even_where_the_rate_is_not_used() {
        let now = Instant::now();
        let mut controller = Controller::new(now, HIGHEST, 0);
        let mut probed = false;
        for tick in 0..600 {
            let at = now + tick * TICK;
            let verdict = controller.step(&Reading {
                now: at,
                written: 0,
                received: Some((at, 0)),
                round_trip: Some(ROUND_TRIP),
                lost: None,
                frames: 1,
                coarse: 1,
                held_up: Duration::ZERO,
                ceiling: HIGHEST,
                patience: PATIENCE,
            });
            probed |= verdict.probing;
        }
        assert!(probed, "an encoder that cannot make do with its rate gets the link probed");
    }

    #[test]
    fn a_link_that_queues_is_not_overrun() {
        let mut session = Session::new(3000);
        session.queues = true;
        session.run(seconds(60));
        assert!((1500..=2900).contains(&session.kbps), "settled at {}", session.kbps);
        assert!(session.worst_delay < seconds(1), "queued for {:?}", session.worst_delay);
        session.capacity = 100_000;
        session.run(seconds(70));
        assert_eq!(session.kbps, HIGHEST);
    }

    #[test]
    fn a_client_that_reports_nothing_keeps_its_rate() {
        let now = Instant::now();
        let mut controller = Controller::new(now, HIGHEST, 0);
        for tick in 0..2000 {
            let verdict = controller.step(&Reading {
                now: now + tick * TICK,
                written: 0,
                received: None,
                round_trip: None,
                lost: None,
                frames: 1,
                coarse: 0,
                held_up: Duration::ZERO,
                ceiling: HIGHEST,
                patience: PATIENCE,
            });
            assert_eq!((verdict.kbps, verdict.padding), (start(HIGHEST), 0));
        }
    }

    #[test]
    fn a_link_that_drops_what_it_does_not_carry_is_held_to_what_arrives() {
        // 4000 kbit/s are sent, half of it gets lost, and nothing is held up or late.
        let (start, mut controller) = (Instant::now(), Controller::new(Instant::now(), HIGHEST, 0));
        let mut kbps = 0;
        for tick in 1..=600u64 {
            let (now, written) = (start + TICK * tick as u32, tick * 2500);
            let verdict = controller.step(&Reading {
                now,
                written,
                received: Some((now, written / 2)),
                round_trip: Some(ROUND_TRIP),
                lost: Some(0.5),
                frames: 1,
                coarse: 0,
                held_up: Duration::ZERO,
                ceiling: HIGHEST,
                patience: PATIENCE,
            });
            kbps = verdict.kbps;
        }
        assert!((1500..=1800).contains(&kbps), "left at {kbps}");
    }

    #[test]
    fn a_long_round_trip_is_not_lag() {
        let mut session = Session::new(100_000);
        session.round_trip = Duration::from_millis(600);
        session.run(seconds(20));
        assert_eq!(session.kbps, HIGHEST);
    }

    #[test]
    fn the_ceiling_is_kept_and_followed() {
        let mut session = Session::new(100_000);
        session.ceiling = 2000;
        session.run(seconds(3));
        assert_eq!(session.kbps, 2000);
        session.ceiling = 10_000;
        session.run(seconds(6));
        assert_eq!(session.kbps, 10_000);
        session.ceiling = 1000;
        session.run(TICK);
        assert_eq!(session.kbps, 1000);
    }

    #[test]
    fn the_shortest_round_trip_is_forgotten_after_a_while() {
        let start = Instant::now();
        let mut fastest = Fastest { recent: None, earlier: None, turned: start };
        fastest.note(start, Some(Duration::from_millis(20)));
        fastest.note(start + MEMORY, Some(Duration::from_millis(300)));
        assert_eq!(fastest.get(), Some(Duration::from_millis(20)));
        fastest.note(start + 2 * MEMORY, Some(Duration::from_millis(300)));
        assert_eq!(fastest.get(), Some(Duration::from_millis(300)));
    }
}
