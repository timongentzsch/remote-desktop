//! The kernel's side of a TCP connection: how much it may hold unsent, what it knows of the path, and which of
//! its congestion controls it uses.

use std::net::TcpStream;
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::time::Duration;

use crate::tls::Control;

/// Lets the kernel hold no more than `bytes` unsent for the connection.
#[cfg(unix)]
pub fn limit_unsent(socket: &TcpStream, bytes: i32) {
    #[cfg(target_os = "linux")]
    use libc::TCP_NOTSENT_LOWAT;
    #[cfg(target_os = "macos")]
    const TCP_NOTSENT_LOWAT: libc::c_int = 0x201; // <netinet/tcp.h>
    set(socket, TCP_NOTSENT_LOWAT, (&raw const bytes).cast(), size_of_val(&bytes));
}

#[cfg(windows)]
pub fn limit_unsent(_socket: &TcpStream, _bytes: i32) {}

/// What the kernel knows of a connection's path: its round trip, and how many segments it sent a second time.
pub struct Path {
    pub round_trip: Duration,
    pub retransmitted: u32,
}

#[cfg(target_os = "linux")]
pub fn path(socket: &TcpStream) -> Option<Path> {
    let mut info = std::mem::MaybeUninit::<libc::tcp_info>::zeroed();
    let mut size = size_of::<libc::tcp_info>() as u32;
    let read = unsafe {
        libc::getsockopt(socket.as_raw_fd(), libc::IPPROTO_TCP, libc::TCP_INFO, info.as_mut_ptr().cast(), &mut size)
    };
    (read == 0).then(|| {
        let info = unsafe { info.assume_init() };
        Path { round_trip: Duration::from_micros(info.tcpi_rtt.into()), retransmitted: info.tcpi_total_retrans }
    })
}

/// Puts the connection on `control`. Where the kernel does not have it, or does not let this user choose it,
/// the connection stays as it is.
#[cfg(target_os = "linux")]
pub fn set_control(socket: &TcpStream, control: Control) {
    let name = match control {
        Control::Bbr => c"bbr",
        Control::Cubic => c"cubic",
    };
    let name = name.to_bytes();
    set(socket, libc::TCP_CONGESTION, name.as_ptr().cast(), name.len());
}

#[cfg(unix)]
fn set(socket: &TcpStream, option: libc::c_int, value: *const libc::c_void, length: usize) {
    unsafe { libc::setsockopt(socket.as_raw_fd(), libc::IPPROTO_TCP, option, value, length as u32) };
}

/// Only Linux currently reads TCP path information and chooses congestion control per connection.
#[cfg(not(target_os = "linux"))]
pub fn path(_socket: &TcpStream) -> Option<Path> {
    None
}

#[cfg(not(target_os = "linux"))]
pub fn set_control(_socket: &TcpStream, _control: Control) {}
