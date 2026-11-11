//! What is waiting to be sent to one client, and in which order it goes out.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::Instant;

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
    /// The piece and when it was queued.
    video: Option<(Vec<u8>, Instant)>,
    audio: VecDeque<Vec<u8>>,
    control: VecDeque<Vec<u8>>,
    closed: bool,
}

impl Outbox {
    /// Waits until the previous piece of video has gone out. False once the session is over.
    pub fn push_video(&self, piece: Vec<u8>) -> bool {
        let mut pending = self
            .changed
            .wait_while(self.pending.lock().unwrap(), |pending| pending.video.is_some() && !pending.closed)
            .unwrap();
        pending.video = Some((piece, Instant::now()));
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

    /// The next thing to send: control messages first, then sound, then video (with the time it was queued).
    /// None once the session is over.
    pub fn next(&self) -> Option<(Vec<u8>, Option<Instant>)> {
        let idle = |pending: &mut Pending| {
            pending.video.is_none() && pending.audio.is_empty() && pending.control.is_empty() && !pending.closed
        };
        let mut pending = self.changed.wait_while(self.pending.lock().unwrap(), idle).unwrap();
        if pending.closed {
            return None;
        }
        let pending = &mut *pending;
        let next = match pending.control.pop_front().or_else(|| pending.audio.pop_front()) {
            Some(message) => (message, None),
            None => pending.video.take().map(|(piece, queued)| (piece, Some(queued)))?,
        };
        self.changed.notify_all();
        Some(next)
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
        // A second piece of video has to wait until the first was taken.
        let second = thread::spawn({
            let outbox = outbox.clone();
            move || outbox.push_video(vec![3])
        });
        let next = || outbox.next().map(|(bytes, queued)| (bytes, queued.is_some()));
        assert_eq!(next(), Some((vec![2], false)));
        assert_eq!(next(), Some((vec![1], true)));
        assert!(second.join().unwrap());
        assert_eq!(next(), Some((vec![3], true)));
        outbox.close();
        assert_eq!(next(), None);
        assert!(!outbox.push_video(vec![4]));
    }
}
