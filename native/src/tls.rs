//! A TLS connection that one thread can read while another writes.

use std::ffi::CStr;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::{ServerConfig, ServerConnection};

/// How much unsent data the kernel may hold for one client. Without a limit it buffers megabytes, which on a
/// slow link is seconds of stale video; with it the writer blocks and the capture skips frames instead.
const UNSENT_LIMIT: libc::c_int = 16 * 1024;

/// The kernel's two ways of deciding how fast to send, by the names it knows them under. Neither suits every
/// path. BBR keeps its speed when packets get lost, but once it has run for ten seconds it all but stops for a
/// fifth of a second, again and again, to measure the round trip afresh, which holds up a dozen frames; only a
/// connection that falls idle between frames, as one with a short round trip does, is spared. Cubic never
/// pauses, but takes every lost packet for congestion: with 0.5% lost at a round trip of 30 ms it carried
/// 4 Mbit/s. So a connection is on BBR while its path is near or has lately lost something, and on Cubic
/// otherwise.
const BBR: &CStr = c"bbr";
const CUBIC: &CStr = c"cubic";
/// The round trip below which BBR's pauses do not show: at 6 ms there were none, at 10 ms there were.
const NEAR: Duration = Duration::from_millis(5);
/// For how long nothing must have been lost for a path to count as losing nothing. Shorter than BBR's ten
/// seconds, so that a path with nothing more than a loss now and then never sees a pause.
const CALM: Duration = Duration::from_secs(8);
const REVIEW: Duration = Duration::from_secs(1);

/// Which of the two suits a path with this round trip on which nothing was lost for this long.
fn suited(round_trip: Duration, calm: Duration) -> &'static CStr {
    if round_trip < NEAR || calm < CALM { BBR } else { CUBIC }
}

/// Keeps a connection on the congestion control that suits its path. The writer calls `review` as it goes.
pub struct Congestion<'a> {
    socket: &'a TcpStream,
    control: &'static CStr,
    reviewed: Instant,
    /// How many segments the kernel had sent a second time at the latest review, and when that last grew. A new
    /// connection counts as having just lost something: it has yet to show that it does not.
    retransmitted: u32,
    lost: Instant,
}

impl Congestion<'_> {
    /// Looks at the path again if it is time to.
    pub fn review(&mut self, now: Instant) {
        if now.duration_since(self.reviewed) < REVIEW {
            return;
        }
        self.reviewed = now;
        let Some(link) = link(self.socket) else { return };
        if link.tcpi_total_retrans != self.retransmitted {
            (self.retransmitted, self.lost) = (link.tcpi_total_retrans, now);
        }
        let control = suited(Duration::from_micros(link.tcpi_rtt.into()), now.duration_since(self.lost));
        if control != self.control {
            self.control = control;
            // Where the kernel does not have the one asked for, or does not let this user choose it, the
            // connection stays as it is.
            let name = control.to_bytes();
            unsafe {
                libc::setsockopt(
                    self.socket.as_raw_fd(),
                    libc::IPPROTO_TCP,
                    libc::TCP_CONGESTION,
                    name.as_ptr().cast(),
                    name.len() as u32,
                )
            };
        }
    }
}

/// What the kernel knows about the connection, of which the round trip (in microseconds) and the count of
/// segments sent again are used.
fn link(socket: &TcpStream) -> Option<libc::tcp_info> {
    let mut link = std::mem::MaybeUninit::<libc::tcp_info>::zeroed();
    let mut size = size_of::<libc::tcp_info>() as u32;
    let read = unsafe {
        libc::getsockopt(socket.as_raw_fd(), libc::IPPROTO_TCP, libc::TCP_INFO, link.as_mut_ptr().cast(), &mut size)
    };
    (read == 0).then(|| unsafe { link.assume_init() })
}

/// One client's encrypted connection. A reader thread and a writer thread share it: the TLS state is locked only
/// while bytes are encrypted or decrypted, never while waiting on the network, so neither holds the other up.
pub struct Connection {
    tls: Mutex<ServerConnection>,
    socket: TcpStream,
    /// Set when reading produced a TLS-level reply that the writer still has to send.
    reply_pending: AtomicBool,
    /// How many milliseconds `read` waits for the client to send something before it gives up.
    patience: AtomicI32,
}

impl Connection {
    /// Does the TLS handshake. The client gets `patience` for it, and then for each wait in `read`.
    pub fn accept(configuration: &Arc<ServerConfig>, mut socket: TcpStream, patience: Duration) -> io::Result<Self> {
        socket.set_nodelay(true)?;
        socket.set_read_timeout(Some(patience))?;
        let limit = UNSENT_LIMIT;
        unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::IPPROTO_TCP,
                libc::TCP_NOTSENT_LOWAT,
                (&raw const limit).cast(),
                size_of::<libc::c_int>() as u32,
            )
        };
        let mut tls = ServerConnection::new(configuration.clone()).map_err(io::Error::other)?;
        tls.set_buffer_limit(None); // `write` hands everything to the socket right away
        while tls.is_handshaking() {
            tls.complete_io(&mut socket)?;
        }
        while tls.wants_write() {
            tls.write_tls(&mut socket)?;
        }
        socket.set_read_timeout(None)?; // from here on `read` does the waiting
        let connection =
            Self { tls: Mutex::new(tls), socket, reply_pending: AtomicBool::new(false), patience: AtomicI32::new(0) };
        connection.set_patience(patience);
        Ok(connection)
    }

    /// The keeper of this connection's congestion control.
    pub fn congestion(&self) -> Congestion<'_> {
        let now = Instant::now();
        Congestion { socket: &self.socket, control: c"", reviewed: now, retransmitted: 0, lost: now }
    }

    /// Changes how long `read` waits for the client to send something.
    pub fn set_patience(&self, patience: Duration) {
        self.patience.store(patience.as_millis() as i32, Ordering::Relaxed);
    }

    /// Encrypts and sends `bytes`. Only one thread may write at a time; an empty slice just sends pending replies.
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut wire = Vec::with_capacity(bytes.len() + 256);
        {
            let mut tls = self.tls.lock().unwrap();
            tls.writer().write_all(bytes)?;
            while tls.wants_write() {
                tls.write_tls(&mut wire)?;
            }
        }
        (&self.socket).write_all(&wire)
    }

    /// Whether a TLS-level reply is waiting for the writer; asking clears the flag.
    pub fn take_reply_pending(&self) -> bool {
        self.reply_pending.swap(false, Ordering::Relaxed)
    }

    pub fn close(&self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}

impl Read for &Connection {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.tls.lock().unwrap().reader().read(buffer) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                result => return result,
            }
            // Nothing decrypted is left: wait for the network without holding the lock the writer needs.
            let patience = Duration::from_millis(self.patience.load(Ordering::Relaxed) as u64);
            if crate::fd::wait_readable([self.socket.as_raw_fd()], Some(patience)) == [false] {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let mut tls = self.tls.lock().unwrap();
            if tls.read_tls(&mut &self.socket)? == 0 {
                return Ok(0);
            }
            tls.process_new_packets().map_err(io::Error::other)?;
            if tls.wants_write() {
                self.reply_pending.store(true, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_path_gets_the_congestion_control_that_suits_it() {
        let (near, far) = (Duration::from_millis(2), Duration::from_millis(30));
        assert_eq!(suited(near, CALM), BBR);
        assert_eq!(suited(far, CALM), CUBIC);
        assert_eq!(suited(far, CALM - REVIEW), BBR);
    }
}
