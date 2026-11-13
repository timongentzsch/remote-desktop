//! The session's video and sound on their way over UDP, with spare datagrams for what the link loses, and
//! what the link's state says about room for more.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::fec;
use crate::quic::{self, Link};

/// Over UDP a piece of video gets spare datagrams for what the link loses (see fec.rs). The link is taken to
/// lose at least this share, however little it lost lately: loss comes in runs.
const LEAST_LOSS: f64 = 0.01;
/// Over UDP the link has room for more when all that it was given up to a round trip and the client's patience
/// ago has arrived or is given up as lost. The round trip is the shortest of late: one that a queue has made
/// longer would make room for that queue. What the rate sends in this many milliseconds may be older and
/// still under way: packets that are lost count as under way until the connection gives them up, which it
/// takes its time over.
const STRAGGLERS: u64 = 30;
/// On a way whose round trip swings by itself, the patience is at least this many times the usual swing: a
/// round trip of 45 to 210 ms, as a relay showed, would else look like a full link most of the time.
const SWINGS: u32 = 4;
/// How long a client that is behind may stand still, a round trip apart, before it is given a frame to begin
/// with.
const STUCK: Duration = Duration::from_millis(300);

/// How much the spares' share keeps of what it was with each piece, so that it follows the last few dozen;
const KEPT: f64 = 0.95;
/// how often the loss is worked out, and how much of a higher loss each new measure keeps.
const MEASURE: Duration = Duration::from_secs(1);
const LOSS_KEPT: f64 = 0.8;

/// The session's video on its way over UDP.
pub(super) struct Datagrams<'a> {
    pub(super) link: &'a Link,
    /// How many bytes were handed to the link, and the number of the next piece.
    pub(super) written: u64,
    pub(super) pieces: u32,
    /// Of the pieces sent lately, how many bytes they had and how many their datagrams took, spares included.
    plain: f64,
    cut: f64,
    /// The share of what was sent that the link lost lately, that share in the last second alone, and the
    /// connection's counts when that was last worked out.
    loss: f64,
    pub(super) lately: f64,
    counted: (Instant, u64, u64),
    /// The piece from which on the frame to begin with that was last asked for makes good what is lost.
    fresh_from: u32,
    /// The number of the next packet of sound, and the packet before it, which goes along once more.
    spoken: (u32, Vec<u8>),
    /// What the client last said of the pieces it has, and since when it should have said something else.
    status: (u32, u32),
    since: Instant,
    /// How much the link had been given in all at moments in the last round trip or so.
    given: VecDeque<(Instant, u64)>,
}

impl<'a> Datagrams<'a> {
    pub(super) fn new(link: &'a Link) -> Self {
        let now = Instant::now();
        Self {
            link,
            written: 0,
            pieces: 0,
            plain: 1.0,
            cut: 1.0,
            loss: 0.0,
            lately: 0.0,
            counted: (now, 0, 0),
            fresh_from: 0,
            spoken: (0, Vec::new()),
            status: link.status(),
            since: now,
            given: VecDeque::new(),
        }
    }

    /// Video changes its way. What the link counted so far, perhaps while nothing got through, is left behind,
    /// and a frame to begin with is on its way.
    pub(super) fn begin(&mut self, now: Instant) {
        (self.fresh_from, self.since, self.loss, self.lately) = (self.pieces, now, 0.0, 0.0);
        self.given.clear();
        if let Some(fared) = self.link.fared(now) {
            self.counted = (now, fared.sent, fared.lost);
        }
    }

    pub(super) fn send(&mut self, datagrams: Vec<Vec<u8>>, kbps: u32) {
        self.written += datagrams.iter().map(|datagram| datagram.len() as u64).sum::<u64>();
        self.link.send(datagrams, kbps);
    }

    pub(super) fn piece(&mut self, piece: &[u8], kbps: u32) {
        if !self.behind() {
            self.since = Instant::now(); // from here on the client has something to catch up with
        }
        let datagrams = fec::cut(self.pieces, piece, self.link.room(), self.loss.max(LEAST_LOSS));
        let took: usize = datagrams.iter().map(Vec::len).sum();
        (self.plain, self.cut) = (KEPT * self.plain + piece.len() as f64, KEPT * self.cut + took as f64);
        self.send(datagrams, kbps);
        self.pieces += 1;
    }

    /// Sends a packet of sound (a message as the connection carries it: the kind, then the packet) ahead of
    /// what waits, and the packet before it once more: one datagram lost then costs nothing. A datagram is the
    /// kind, the packet's number, its length, the packet, and the one before.
    pub(super) fn sound(&mut self, message: &[u8], kbps: u32) {
        let (kind, packet) = (message[0], &message[1..]);
        let (number, before) = &mut self.spoken;
        let head = [&[kind][..], &number.to_be_bytes(), &(packet.len() as u16).to_be_bytes()].concat();
        let datagram = [&head[..], packet, before].concat();
        (*number, *before) = (*number + 1, packet.to_vec());
        self.written += datagram.len() as u64;
        self.link.send_ahead(datagram, kbps);
    }

    /// The share of the rate that is left for video itself, beside its spares and what datagrams begin with.
    pub(super) fn share(&self) -> f64 {
        self.plain / self.cut
    }

    /// Works out anew, once a second, how much the link loses. A rise counts at once; it is forgotten slowly.
    pub(super) fn measure(&mut self, now: Instant) {
        let (at, sent, lost) = self.counted;
        if now.duration_since(at) >= MEASURE
            && let Some(fared) = self.link.fared(now)
        {
            // Another connection may have taken the link, whose counts begin anew.
            let (more_sent, more_lost) = (fared.sent.saturating_sub(sent), fared.lost.saturating_sub(lost));
            let lately = more_lost as f64 / (more_sent as f64).max(1.0);
            (self.loss, self.lately) = (lately.max(LOSS_KEPT * self.loss), lately);
            self.counted = (now, fared.sent, fared.lost);
        }
    }

    /// Whether the link has taken what it was given, but for the latest and for stragglers.
    pub(super) fn has_room(&mut self, now: Instant, kbps: u32, patience: Duration) -> bool {
        let Some(fared) = self.link.fared(now) else { return true };
        self.given.push_back((now, self.written));
        // Of the moments noted, the latest that is long enough ago says what should be done with by now.
        let long_ago = |at: Instant| now.duration_since(at) >= fared.shortest + patience.max(SWINGS * fared.swing);
        while self.given.get(1).is_some_and(|&(at, _)| long_ago(at)) {
            self.given.pop_front();
        }
        let done = self.written.saturating_sub(fared.under_way) + kbps as u64 * 125 * STRAGGLERS / 1000;
        self.given.front().is_none_or(|&(at, given)| !long_ago(at) || done >= given)
    }

    /// Whether the client has not got all that was sent.
    pub(super) fn behind(&self) -> bool {
        let (used, missing) = self.status;
        missing != quic::NOTHING || (self.pieces > 0 && used != self.pieces - 1)
    }

    /// Whether the client needs a frame to begin with that has not been asked for yet: it says a piece is
    /// missing, or it is behind and has stood still for too long. That is how it shows when the latest piece
    /// got lost, after which nothing comes that would show the gap, or the frame to begin with itself.
    pub(super) fn needs_fresh(&mut self, now: Instant, round_trip: Duration) -> bool {
        let status = self.link.status();
        if status != self.status {
            (self.status, self.since) = (status, now);
        }
        let gap = status.1 != quic::NOTHING && status.1 >= self.fresh_from;
        let stuck = self.behind() && now.duration_since(self.since) > STUCK + round_trip;
        if gap || stuck {
            (self.fresh_from, self.since) = (self.pieces, now);
        }
        gap || stuck
    }
}
