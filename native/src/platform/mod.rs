//! Everything that differs from one operating system to another, behind the one surface the rest of the host
//! uses. Each system is a module here that provides these same names, chosen when the host is built; Linux is
//! the only one so far. The rest of the host (the server and sessions, the bitrate, QUIC, the stream and its
//! encoders, and the client) is the same everywhere.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as os;

#[cfg(not(target_os = "linux"))]
compile_error!("there is no platform module for this operating system yet: see platform/mod.rs");

// The screen: its picture for a session (`capture(settings, shared, sink)`, see capture.rs), the screens there
// are (`screens()`), waking those that sleep (`wake_displays(&input)`), a screen made to the client's window
// where the desktop can make one (`virtual_screen::{VirtualScreen, Window, NAME}`), the desktop's pointer
// hidden while a client draws its own where the picture would show it too (`hide_pointer()`), and undoing
// what a host that crashed left changed (`restore_leftovers()`).
pub use os::capture::run as capture;
pub use os::displays::wake_displays;
pub use os::pointer::hide as hide_pointer;
pub use os::virtual_screen;
pub use os::{restore_leftovers, screens};

// What the client does: keys, buttons and the pointer, in the client's codes, which are Linux's (linux/input.rs).
pub use os::input::Input;

// What the session sends along: the desktop's sound, 48 kHz stereo samples that `record_sound(shared, sound)`
// hands to the encoder (audio.rs), and clipboard text both ways (`clipboard::{Inbox, run}`).
pub use os::audio::run as record_sound;
pub use os::clipboard;

// Setting the host up: the `install`, `uninstall`, `password` and `certificate` commands, and the host at the
// login screen (`login_screen(address)`).
pub use os::greeter::run as login_screen;
pub use os::install;

// Where the password and certificate are kept, and the machine's name.
pub use os::{data_directory, hostname, password_path, write_private};

// Waiting on a socket (`socket_readable`), and on a socket or a wake from another thread (`Waker`); the
// kernel's side of a TCP connection (`tcp`).
pub use os::fd::{Waker, socket_readable};
pub use os::tcp;
