//! Remote desktop host in one binary: serves the web client, streams the screen as H.264 and injects the
//! client's keyboard and pointer input.
//!
//!     remote-desktop install [--tailscale]      install system-wide and run as a service of the desktop session
//!     remote-desktop uninstall                  remove what install set up
//!     remote-desktop password [--none]          set the login password, or allow access without one
//!     remote-desktop certificate [--tailscale]  make a new certificate, or switch where it comes from
//!     remote-desktop [--listen ADDRESS]         run the host (default 0.0.0.0:8765)

mod audio;
mod capture;
mod certificate;
mod clipboard;
mod compositor;
mod gnome;
mod input;
mod install;
mod outbox;
mod rate;
mod screen;
mod server;
mod session;
mod stream;
mod tls;
mod ws;
mod xserver;

const PORT: u16 = 8765;

fn fail(message: &str) -> ! {
    eprintln!("remote-desktop: {message}");
    std::process::exit(1)
}

/// Whether an optional argument is absent or the one option these commands take.
fn is_tailscale(option: &Option<String>) -> bool {
    option.as_deref().is_none_or(|option| option == "--tailscale")
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    match (arguments.next().as_deref(), arguments.next()) {
        (None, _) => server::run(&format!("0.0.0.0:{PORT}")),
        (Some("--listen"), Some(address)) => server::run(&address),
        (Some("install"), option) if is_tailscale(&option) => install::install(option.is_some()),
        (Some("uninstall"), None) => install::uninstall(),
        (Some("password"), None) => install::set_password(),
        (Some("password"), Some(option)) if option == "--none" => install::remove_password(),
        (Some("certificate"), option) if is_tailscale(&option) => install::renew_certificate(option.is_some()),
        // Not for users: the service runs this once to learn whether the GPU can encode (see stream/encode.rs).
        (Some(stream::PROBE), None) => std::process::exit(if stream::probe() { 0 } else { 1 }),
        _ => fail(
            "usage: remote-desktop [install [--tailscale] | uninstall | password [--none] | certificate [--tailscale] | --listen ADDRESS]",
        ),
    }
}
