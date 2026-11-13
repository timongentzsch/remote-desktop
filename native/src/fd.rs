//! Waiting on file descriptors, and waking a thread that waits on one.

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

/// A descriptor that one thread makes readable to wake another waiting on it (see `wait_readable`).
pub struct Waker(OwnedFd);

impl Waker {
    pub fn new() -> io::Result<Self> {
        match unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) } {
            -1 => Err(io::Error::last_os_error()),
            fd => Ok(Self(unsafe { OwnedFd::from_raw_fd(fd) })),
        }
    }

    /// Makes it readable until `settle` is called.
    pub fn wake(&self) {
        let one = 1u64.to_ne_bytes();
        unsafe { libc::write(self.0.as_raw_fd(), one.as_ptr().cast(), one.len()) };
    }

    /// Takes back every `wake` so far.
    pub fn settle(&self) {
        let mut count = [0u8; 8];
        unsafe { libc::read(self.0.as_raw_fd(), count.as_mut_ptr().cast(), count.len()) };
    }

    pub fn fd(&self) -> RawFd {
        self.0.as_raw_fd()
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
