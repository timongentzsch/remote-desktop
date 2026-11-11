//! A TLS connection that one thread can read while another writes.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rustls::{ServerConfig, ServerConnection};

/// How much unsent data the kernel may hold for one client. Without a limit it buffers megabytes, which on a
/// slow link is seconds of stale video; with it the writer blocks and the capture skips frames instead.
const UNSENT_LIMIT: libc::c_int = 16 * 1024;

/// One client's encrypted connection. A reader thread and a writer thread share it: the TLS state is locked only
/// while bytes are encrypted or decrypted, never while waiting on the network, so neither holds the other up.
pub struct Connection {
    tls: Mutex<ServerConnection>,
    socket: TcpStream,
    /// Set when reading produced a TLS-level reply that the writer still has to send.
    reply_pending: AtomicBool,
}

impl Connection {
    pub fn accept(configuration: &Arc<ServerConfig>, mut socket: TcpStream) -> io::Result<Self> {
        socket.set_nodelay(true)?;
        let limit = UNSENT_LIMIT;
        unsafe { libc::setsockopt(socket.as_raw_fd(), libc::IPPROTO_TCP, libc::TCP_NOTSENT_LOWAT, (&raw const limit).cast(), size_of::<libc::c_int>() as u32) };
        let mut tls = ServerConnection::new(configuration.clone()).map_err(io::Error::other)?;
        tls.set_buffer_limit(None); // `write` hands everything to the socket right away
        while tls.is_handshaking() {
            tls.complete_io(&mut socket)?;
        }
        while tls.wants_write() {
            tls.write_tls(&mut socket)?;
        }
        Ok(Self { tls: Mutex::new(tls), socket, reply_pending: AtomicBool::new(false) })
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
            let mut poll = libc::pollfd { fd: self.socket.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            unsafe { libc::poll(&mut poll, 1, -1) };
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
