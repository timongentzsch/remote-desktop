//! Waiting on file descriptors, and waking a thread that waits on one: `poll` and a pipe, which every Unix has.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

/// Waits until one of `fds` can be read, for up to `limit` or else for good, and says which can. A negative
/// one is skipped. A limit is rounded up to the next millisecond, so that a waiting loop does not spin through
/// the last fraction of one.
pub fn wait_readable<const N: usize>(fds: [RawFd; N], limit: Option<Duration>) -> [bool; N] {
    let mut polled = fds.map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 });
    let millis = limit.map_or(-1, |limit| limit.as_nanos().div_ceil(1_000_000).min(i32::MAX as u128) as i32);
    unsafe { libc::poll(polled.as_mut_ptr(), N as libc::nfds_t, millis) };
    polled.map(|polled| polled.revents != 0)
}

/// Waits until `socket` can be read, for up to `limit`, and says whether it can.
pub fn socket_readable(socket: &impl AsRawFd, limit: Duration) -> bool {
    wait_readable([socket.as_raw_fd()], Some(limit)) == [true]
}

/// A pipe whose end one thread makes readable to wake another waiting on it (see `wait_readable`).
pub struct Waker {
    read: OwnedFd,
    write: OwnedFd,
}

impl Waker {
    pub fn new() -> io::Result<Self> {
        let mut ends = [0; 2];
        if unsafe { libc::pipe(ends.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
        // Neither end may block, and neither is passed on to a program this one starts.
        for end in [&read, &write] {
            let fd = end.as_raw_fd();
            unsafe {
                libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK);
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
        Ok(Self { read, write })
    }

    /// Makes it readable until `settle` is called. A full pipe is as readable as it gets.
    pub fn wake(&self) {
        unsafe { libc::write(self.write.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
    }

    /// Waits until `socket` can be read or `wake` is called, for up to `limit`, and takes back every `wake`.
    pub fn wait(&self, socket: &impl AsRawFd, limit: Duration) {
        wait_readable([socket.as_raw_fd(), self.fd()], Some(limit));
        self.settle();
    }

    /// Takes back every `wake` so far.
    pub fn settle(&self) {
        let mut drained = [0u8; 64];
        while unsafe { libc::read(self.read.as_raw_fd(), drained.as_mut_ptr().cast(), drained.len()) } > 0 {}
    }

    pub fn fd(&self) -> RawFd {
        self.read.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_waker_wakes_until_it_settles() {
        let waker = Waker::new().unwrap();
        let quick = Some(Duration::ZERO);
        assert_eq!(wait_readable([waker.fd()], quick), [false]);
        waker.wake();
        waker.wake();
        assert_eq!(wait_readable([waker.fd(), -1], quick), [true, false]);
        waker.settle();
        assert_eq!(wait_readable([waker.fd()], quick), [false]);
    }
}
