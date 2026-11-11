//! Remote desktop host in one binary: serves the web client, streams the screen as H.264 and injects the
//! client's keyboard and pointer input.
//!
//!     remote-desktop install             install system-wide and run as a service of the desktop session
//!     remote-desktop password            set the login password
//!     remote-desktop [--listen ADDRESS]  run the host (default 127.0.0.1:8765)
//!
//! It listens on localhost only by default. Browsers expose video decoding and keyboard capture only on secure
//! origins, and localhost is one: reach it with `ssh -NL 8765:localhost:8765 user@host` and open
//! http://localhost:8765 in Chrome.

mod capture;
mod convert;
mod encode;
mod input;
mod install;
mod server;
mod ws;

fn fail(message: &str) -> ! {
    eprintln!("remote-desktop: {message}");
    std::process::exit(1)
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    match (arguments.next().as_deref(), arguments.next()) {
        (None, _) => server::run("127.0.0.1:8765"),
        (Some("--listen"), Some(address)) => server::run(&address),
        (Some("install"), None) => install::install(),
        (Some("password"), None) => install::set_password(),
        _ => fail("usage: remote-desktop [install | password | --listen ADDRESS]"),
    }
}
