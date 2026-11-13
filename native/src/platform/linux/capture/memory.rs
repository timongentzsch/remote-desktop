//! Memory shared with the display server, which copies the screen's picture into it.

use std::os::fd::{FromRawFd, OwnedFd};

pub struct Memory {
    /// What the display server is handed to reach the same memory.
    pub file: OwnedFd,
    map: *mut u8,
    length: usize,
}

impl Memory {
    pub fn new(length: usize) -> Result<Self, String> {
        unsafe {
            let fd = libc::memfd_create(c"remote-desktop".as_ptr(), libc::MFD_CLOEXEC);
            if fd < 0 {
                return Err("could not allocate the capture buffer".into());
            }
            let file = OwnedFd::from_raw_fd(fd);
            if libc::ftruncate(fd, length as libc::off_t) < 0 {
                return Err("could not size the capture buffer".into());
            }
            let map =
                libc::mmap(std::ptr::null_mut(), length, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
            if map == libc::MAP_FAILED {
                return Err("could not map the capture buffer".into());
            }
            Ok(Self { file, map: map.cast(), length })
        }
    }

    /// The memory's contents. The display server writes them only while it answers a request for a picture,
    /// and they are looked at only after its answer.
    pub fn bytes(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.map, self.length) }
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.map.cast(), self.length) };
    }
}
