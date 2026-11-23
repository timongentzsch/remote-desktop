//! Waiting on sockets, and waking a thread that waits on one: WSAPoll, and a loopback socket that sends itself
//! a byte, as Windows has no pipe that WSAPoll waits on.

use std::io;
use std::net::{Ipv4Addr, UdpSocket};
use std::os::windows::io::{AsRawSocket, RawSocket};
use std::time::Duration;

use windows::Win32::Networking::WinSock::{POLLRDNORM, SOCKET, WSAPOLLFD, WSAPoll};

/// Waits until one of `sockets` can be read, for up to `limit`, and says which can. A limit is rounded up to
/// the next millisecond, so that a waiting loop does not spin through the last fraction of one.
fn wait_readable<const N: usize>(sockets: [RawSocket; N], limit: Duration) -> [bool; N] {
    let mut polled = sockets.map(|socket| WSAPOLLFD {
        fd: SOCKET(socket as usize),
        events: POLLRDNORM,
        revents: Default::default(),
    });
    let millis = limit.as_nanos().div_ceil(1_000_000).min(i32::MAX as u128) as i32;
    unsafe { WSAPoll(polled.as_mut_ptr(), N as u32, millis) };
    polled.map(|polled| polled.revents.0 != 0)
}

/// Waits until `socket` can be read, for up to `limit`, and says whether it can.
pub fn socket_readable(socket: &impl AsRawSocket, limit: Duration) -> bool {
    wait_readable([socket.as_raw_socket()], limit) == [true]
}

/// A socket on the loopback address that one thread sends a byte to itself on, to wake another waiting on it.
pub struct Waker {
    socket: UdpSocket,
}

impl Waker {
    pub fn new() -> io::Result<Self> {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
        socket.connect(socket.local_addr()?)?;
        socket.set_nonblocking(true)?;
        Ok(Self { socket })
    }

    /// Makes it readable until `settle` is called. A full buffer is as readable as it gets.
    pub fn wake(&self) {
        let _ = self.socket.send(&[1]);
    }

    /// Waits until `socket` can be read or `wake` is called, for up to `limit`, and takes back every `wake`.
    pub fn wait(&self, socket: &impl AsRawSocket, limit: Duration) {
        wait_readable([socket.as_raw_socket(), self.socket.as_raw_socket()], limit);
        self.settle();
    }

    /// Takes back every `wake` so far.
    pub fn settle(&self) {
        let mut drained = [0u8; 64];
        while self.socket.recv(&mut drained).is_ok() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_waker_wakes_until_it_settles() {
        let waker = Waker::new().unwrap();
        let readable = || wait_readable([waker.socket.as_raw_socket()], Duration::ZERO) == [true];
        assert!(!readable());
        waker.wake();
        waker.wake();
        std::thread::sleep(Duration::from_millis(10)); // the loopback hands the bytes over
        assert!(readable());
        waker.settle();
        assert!(!readable());
    }
}
