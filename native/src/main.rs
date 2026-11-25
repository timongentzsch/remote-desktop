//! Remote desktop host in one binary: serves the web client, streams the screen as H.264 and injects the
//! client's keyboard and pointer input.
//!
//!     remote-desktop install [--tailscale] [--no-password] [--port=PORT]
//!                                               install system-wide and run as a service of the desktop session;
//!                                               with --tailscale, for devices on the tailnet only
//!     remote-desktop uninstall                  remove what install set up
//!     remote-desktop password [--none]          set the login password, or allow access without one
//!     remote-desktop certificate [--tailscale]  make a new certificate, or switch where it comes from
//!     remote-desktop [--listen ADDRESS]         run the host (default 0.0.0.0:443, or the port installed with)
//!     remote-desktop login-screen [ADDRESS]     as root: run the host at the login screen whenever there is one

// macOS tells the client no pointer shapes yet, and only Linux asks the kernel about a connection's path, which
// leaves what serves them unused elsewhere.
#![cfg_attr(any(target_os = "macos", windows), allow(dead_code))]

/// Tells what happened on standard error, which the service's log keeps, under the program's name.
macro_rules! warn {
    ($($message:tt)*) => {
        eprintln!("{}: {}", $crate::NAME, format_args!($($message)*))
    };
}

mod audio;
mod capture;
mod certificate;
mod fec;
mod install;
mod outbox;
mod platform;
mod quic;
mod rate;
mod screen;
mod server;
mod session;
mod stream;
mod tls;
mod typing;
mod ws;

/// The program's name: that of its binary, its services and the directory its settings are kept in.
const NAME: &str = "remote-desktop";
/// The host's port, for TCP and for UDP, unless `install --port=` chose another.
const PORT: u16 = 443;
const USAGE: &str = "usage: remote-desktop [install [--tailscale] [--no-password] [--port=PORT] | uninstall | \
                     password [--none] | certificate [--tailscale] | --listen ADDRESS]";

/// Ends the program with what went wrong.
fn fail(message: &str) -> ! {
    warn!("{message}");
    std::process::exit(1)
}

/// Where `install --port=` keeps its choice: with the password, so that installing again keeps it too.
fn port_path() -> std::path::PathBuf {
    platform::data_directory().join("port")
}

/// The port the installed host has.
fn port() -> u16 {
    std::fs::read_to_string(port_path()).ok().and_then(|port| port.trim().parse().ok()).unwrap_or(PORT)
}

/// Where the host listens unless told otherwise: on every interface.
fn listen_everywhere() -> String {
    format!("0.0.0.0:{}", port())
}

/// Whether an optional argument is absent or the one option these commands take.
fn is_tailscale(option: &Option<String>) -> bool {
    option.as_deref().is_none_or(|option| option == "--tailscale")
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    match (arguments.next().as_deref(), arguments.next()) {
        (None, _) => server::run(&listen_everywhere()),
        (Some("--listen"), Some(address)) => server::run(&address),
        // What the system service that `install` sets up runs.
        (Some("login-screen"), address) => platform::login_screen(&address.unwrap_or_else(listen_everywhere)),
        (Some("install"), first) => {
            let options: Vec<String> = first.into_iter().chain(arguments).collect();
            let given = |option: &str| options.iter().any(|given| given == option);
            let port = options.iter().find_map(|option| option.strip_prefix("--port="));
            let port = port.map(|port| port.parse().ok().filter(|&port| port != 0).unwrap_or_else(|| fail(USAGE)));
            let known = |option: &String| ["--tailscale", "--no-password"].contains(&option.as_str());
            if options.iter().any(|option| !known(option) && !option.starts_with("--port=")) {
                fail(USAGE);
            }
            install::install(given("--tailscale"), given("--no-password"), port)
        }
        (Some("uninstall"), None) => install::uninstall(),
        (Some("password"), None) => install::set_password(),
        (Some("password"), Some(option)) if option == "--none" => install::remove_password(),
        (Some("certificate"), option) if is_tailscale(&option) => install::renew_certificate(option.is_some()),
        // Not for users: what makes the virtual display on macOS (see platform/macos/virtual_screen.rs).
        #[cfg(target_os = "macos")]
        (Some(platform::virtual_screen::HELPER), Some(width)) => {
            platform::virtual_screen::helper(std::iter::once(width).chain(arguments))
        }
        // Not for users: the service runs this once to learn whether the GPU can encode (see stream/encode.rs).
        (Some(stream::PROBE), None) => std::process::exit(if stream::probe() { 0 } else { 1 }),
        _ => fail(USAGE),
    }
}
