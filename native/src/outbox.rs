//! What is waiting to be sent to one client, and in which order it goes out.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// The queue between a session's sources and its writer. It holds at most one piece of video, so a slow link makes the
/// capture skip frames instead of queueing them, while control messages and sound are never stuck behind video.
/// Sound that cannot leave in time is dropped: late sound is worse than a gap.
#[derive(Default)]
pub struct Outbox {
    pending: Mutex<Pending>,
    changed: Condvar,
}

#[derive(Default)]
struct Pending {
    /// The piece and when it was ready.
    video: Option<(Vec<u8>, Instant)>,
    /// For how long video has been held up since that was last asked, each piece waiting for the one before
    /// it to be taken, and since when a piece is waiting right now.
    held_up: Duration,
    waiting_since: Option<Instant>,
    audio: VecDeque<Vec<u8>>,
    control: VecDeque<Vec<u8>>,
    closed: bool,
}

/// What `Outbox::next` found.
#[derive(Debug, PartialEq)]
pub enum Next {
    /// Bytes to send; for a piece of video, also when it was ready.
    Send(Vec<u8>, Option<Instant>),
    /// Nothing came up in the time given.
    Nothing,
    /// The session is over.
    Closed,
}

impl Outbox {
    /// Waits until the previous piece of video has gone out. False once the session is over.
    pub fn push_video(&self, piece: Vec<u8>) -> bool {
        let ready = Instant::now();
        let mut pending = self.pending.lock().unwrap();
        if pending.video.is_some() && !pending.closed {
            pending.waiting_since = Some(ready);
            pending = self.changed.wait_while(pending, |pending| pending.video.is_some() && !pending.closed).unwrap();
            let waited = pending.waiting_since.take().map_or(Duration::ZERO, |since| since.elapsed());
            pending.held_up += waited;
        }
        pending.video = Some((piece, ready));
        self.changed.notify_all();
        !pending.closed
    }

    /// Queues a packet of sound, dropping the oldest when more than a fifth of a second is waiting.
    pub fn push_audio(&self, packet: Vec<u8>) {
        let mut pending = self.pending.lock().unwrap();
        if pending.audio.len() >= 10 {
            pending.audio.pop_front();
        }
        pending.audio.push_back(packet);
        self.changed.notify_all();
    }

    pub fn push_control(&self, message: Vec<u8>) {
        self.pending.lock().unwrap().control.push_back(message);
        self.changed.notify_all();
    }

    /// The next thing to send, waiting up to `patience` for one: control messages first, then sound, then video,
    /// unless `video` says that the way it takes has no room for it now.
    pub fn next(&self, patience: Duration, video: bool) -> Next {
        let idle = |pending: &mut Pending| {
            (pending.video.is_none() || !video)
                && pending.audio.is_empty()
                && pending.control.is_empty()
                && !pending.closed
        };
        let (mut pending, _) = self.changed.wait_timeout_while(self.pending.lock().unwrap(), patience, idle).unwrap();
        if pending.closed {
            return Next::Closed;
        }
        let pending = &mut *pending;
        let next = match pending.control.pop_front().or_else(|| pending.audio.pop_front()) {
            Some(message) => Next::Send(message, None),
            None => match pending.video.take_if(|_| video) {
                Some((piece, ready)) => Next::Send(piece, Some(ready)),
                None => return Next::Nothing,
            },
        };
        self.changed.notify_all();
        next
    }

    /// For how long video was held up since this was last asked: time in which the capture could not hand a
    /// piece over, and so did not capture, because the link is slower than the stream.
    pub fn take_held_up(&self) -> Duration {
        let mut pending = self.pending.lock().unwrap();
        let mut held_up = std::mem::take(&mut pending.held_up);
        if let Some(since) = &mut pending.waiting_since {
            held_up += since.elapsed();
            *since = Instant::now();
        }
        held_up
    }

    pub fn close(&self) {
        self.pending.lock().unwrap().closed = true;
        self.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::*;

    #[test]
    fn outbox_sends_control_before_video_and_holds_one_piece() {
        let outbox = Arc::new(Outbox::default());
        assert!(outbox.push_video(vec![1]));
        outbox.push_control(vec![2]);
        // A second piece of video has to wait until the first was taken, and its wait is counted as it goes.
        let second = thread::spawn({
            let outbox = outbox.clone();
            move || outbox.push_video(vec![3])
        });
        while outbox.take_held_up().is_zero() {
            thread::yield_now();
        }
        // What comes next and whether it is video.
        let next = || match outbox.next(Duration::from_millis(10), true) {
            Next::Send(bytes, ready) => Some((bytes, ready.is_some())),
            other => {
                assert_eq!(other, Next::Nothing);
                None
            }
        };
        assert_eq!(outbox.next(Duration::ZERO, false), Next::Send(vec![2], None));
        assert_eq!(outbox.next(Duration::ZERO, false), Next::Nothing, "video waits where it is told to");
        assert_eq!(next(), Some((vec![1], true)));
        assert!(second.join().unwrap());
        assert_eq!(next(), Some((vec![3], true)));
        assert_eq!(next(), None, "an empty outbox is not waited on for longer than asked");
        outbox.close();
        assert_eq!(outbox.next(Duration::from_secs(60), true), Next::Closed);
        assert!(!outbox.push_video(vec![4]));
    }
}
