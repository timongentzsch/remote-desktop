//! What Linux and macOS do alike, as Unix systems: waiting on file descriptors, files only their owner reads,
//! the machine's name, and typing without echo.

pub mod fd;

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Writes a file only its owner can read, creating the directory it lives in.
pub fn write_private(path: &Path, contents: &[u8]) -> Result<(), String> {
    fs::create_dir_all(path.parent().unwrap())
        .and_then(|()| fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path))
        .and_then(|mut file| file.write_all(contents))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// Runs `action` with terminal echo off, if standard input is a terminal.
pub fn without_echo<T>(action: impl FnOnce() -> T) -> T {
    let mut terminal = unsafe { std::mem::zeroed::<libc::termios>() };
    let is_terminal = unsafe { libc::tcgetattr(0, &mut terminal) } == 0;
    if is_terminal {
        let mut silent = terminal;
        silent.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &silent) };
    }
    let result = action();
    if is_terminal {
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &terminal) };
    }
    result
}

/// The machine's name, or nothing where it has none.
pub fn hostname() -> String {
    let mut buffer = [0u8; 256];
    match unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len() - 1) } {
        0 => String::from_utf8_lossy(&buffer[..buffer.iter().position(|&byte| byte == 0).unwrap_or(0)]).into_owned(),
        _ => String::new(),
    }
}
