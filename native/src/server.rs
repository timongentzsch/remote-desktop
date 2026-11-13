//! The network side: serves the client page over HTTPS and hands each WebSocket connection to a session.

use std::fs;
use std::io::{self, ErrorKind, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use rustls::ServerConfig;

use crate::input::Input;
use crate::tls::Connection;
use crate::{certificate, fail, install, quic, session, virtual_screen, ws};

/// The client, compiled into the binary: path, content type, body.
const PAGES: [(&str, &str, &[u8]); 5] = [
    ("/", "text/html", include_bytes!("../../index.html")),
    ("/manifest.json", "application/manifest+json", include_bytes!("../../manifest.json")),
    ("/icon.svg", "image/svg+xml", include_bytes!("../../icon.svg")),
    ("/JetBrainsMono-Regular.woff2", FONT, include_bytes!("../../fonts/JetBrainsMono-Regular.woff2")),
    ("/JetBrainsMono-Medium.woff2", FONT, include_bytes!("../../fonts/JetBrainsMono-Medium.woff2")),
];
/// The typeface does not change from one build to the next as the page does, so a browser may keep it a week.
const FONT: &str = "font/woff2";
const FONT_KEEPING: &str = "max-age=604800";
/// The most a request's head may take before it is turned away.
const LONGEST_HEAD: usize = 8192;

/// How long a client may take over each step before it has logged in. Without a limit, connections that say
/// nothing would each keep a thread waiting for good.
const LOGIN_PATIENCE: Duration = Duration::from_secs(10);
/// How long the port may stay taken at the start. The host of a desktop session and the one at the login
/// screen follow each other on it, and the one that leaves needs a moment.
const PORT_PATIENCE: Duration = Duration::from_secs(20);
const PORT_RETRY: Duration = Duration::from_millis(250);

/// Tailscale's addresses (see `on_tailnet`): 100.64.0.0/10, and fd7a:115c:a1e0::/48.
const TAILNET_V4: (Ipv4Addr, u32) = (Ipv4Addr::new(100, 64, 0, 0), 10);
const TAILNET_V6: (Ipv6Addr, u32) = (Ipv6Addr::new(0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 0), 48);

/// What all connections share.
pub struct Host {
    pub input: Input,
    /// Empty when the owner chose to allow access without a password.
    pub password: Vec<u8>,
    /// With a certificate from Tailscale, only devices on the tailnet are answered (see `on_tailnet`).
    pub tailnet_only: bool,
    /// Replaced once a day, so that a renewed certificate is picked up without a restart.
    tls: Mutex<Arc<ServerConfig>>,
    /// The UDP side, once the host serves and if the UDP port could be had.
    pub udp: OnceLock<quic::Endpoint>,
}

impl Host {
    /// Reads the password and the certificate and creates the input devices.
    pub fn open() -> Arc<Self> {
        let password = fs::read(install::password_path())
            .unwrap_or_else(|_| fail("no password is set; run `remote-desktop password`"))
            .trim_ascii()
            .to_vec();
        let input = Input::open().unwrap_or_else(|error| {
            fail(&format!("cannot create input devices through /dev/uinput ({error}); run `remote-desktop install`"))
        });
        let tls = certificate::configuration().unwrap_or_else(|error| fail(&error));
        let tailnet_only = certificate::tailscale_name().is_some();
        Arc::new(Self { input, password, tailnet_only, tls: Mutex::new(tls), udp: OnceLock::new() })
    }
}

/// The host of a desktop session: serves it for as long as the session lasts.
pub fn run(address: &str) -> ! {
    virtual_screen::restore_leftover();
    let host = Host::open();
    thread::spawn({
        let host = host.clone();
        move || loop {
            thread::sleep(certificate::RENEWAL);
            match certificate::configuration() {
                Ok(fresh) => *host.tls.lock().unwrap() = fresh,
                Err(error) => warn!("could not reload the certificate: {error}"),
            }
        }
    });
    serve(listen(address), host)
}

/// Whether a connection comes from this machine or from a device on the tailnet: Tailscale's addresses, which
/// it drops itself when they arrive on any other interface than its own, so that nobody on the local network
/// can pose as a device on the tailnet. A subnet router's forwarding arrives from the local network and is
/// not on the tailnet in this sense.
pub fn on_tailnet(address: IpAddr) -> bool {
    match address.to_canonical() {
        IpAddr::V4(address) => {
            let (network, length) = TAILNET_V4;
            address.is_loopback() || (address.to_bits() ^ network.to_bits()) >> (32 - length) == 0
        }
        IpAddr::V6(address) => {
            let (network, length) = TAILNET_V6;
            address.is_loopback() || (address.to_bits() ^ network.to_bits()) >> (128 - length) == 0
        }
    }
}

/// Takes the port, waiting a while if another host still has it.
pub fn listen(address: &str) -> TcpListener {
    let began = Instant::now();
    loop {
        match TcpListener::bind(address) {
            Ok(listener) => return listener,
            Err(error) if error.kind() == ErrorKind::AddrInUse && began.elapsed() < PORT_PATIENCE => {
                thread::sleep(PORT_RETRY)
            }
            Err(error) => fail(&format!("cannot listen on {address}: {error}")),
        }
    }
}

/// Answers every connection, each on a thread of its own. First it takes the UDP port of the listener's number
/// for the host's UDP side; without that one, video stays on TCP.
pub fn serve(listener: TcpListener, host: Arc<Host>) -> ! {
    match listener.local_addr().and_then(|address| quic::Endpoint::open(&address.to_string(), host.tailnet_only)) {
        Ok(udp) => drop(host.udp.set(udp)),
        Err(error) => warn!("no UDP side, video stays on TCP: {error}"),
    }
    if host.tailnet_only {
        warn!("answering devices on the tailnet only");
    }
    for socket in listener.incoming().flatten() {
        if host.tailnet_only && !socket.peer_addr().is_ok_and(|peer| on_tailnet(peer.ip())) {
            continue; // closed unanswered
        }
        let host = host.clone();
        thread::spawn(move || {
            let _ = answer(socket, &host); // an error here is the client going away
        });
    }
    unreachable!("the listener never stops accepting")
}

fn answer(socket: TcpStream, host: &Arc<Host>) -> io::Result<()> {
    let configuration = host.tls.lock().unwrap().clone();
    let connection = Arc::new(Connection::accept(&configuration, socket, LOGIN_PATIENCE)?);
    let (mut head, began) = (Vec::new(), Instant::now());
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        (&*connection).read_exact(&mut byte)?;
        head.push(byte[0]);
        if head.len() > LONGEST_HEAD || began.elapsed() > LOGIN_PATIENCE {
            return Ok(());
        }
    }
    let request = String::from_utf8_lossy(&head);
    let path = request.split(' ').nth(1).unwrap_or("");
    let header = |wanted: &str| {
        request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case(wanted).then(|| value.trim())
        })
    };
    if let (true, Some(key)) = (path == "/ws", header("sec-websocket-key")) {
        // Browsers let any page open a WebSocket to any server and say in Origin which page is asking. Only
        // this server's own page may: another site must not reach the desktop through a visitor's browser.
        let own = format!("https://{}", header("host").unwrap_or(""));
        if header("origin").is_some_and(|origin| origin != own) {
            return connection.write(&empty_response("403 Forbidden"));
        }
        let accept = ws::accept_key(key);
        connection.write(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes())?;
        session::run(&connection, host);
        return Ok(());
    }
    let response = match PAGES.iter().find(|page| page.0 == path) {
        Some((_, content_type, body)) => {
            // The page hides its password field when none is needed.
            let open = path == "/" && host.password.is_empty();
            let body = if open {
                String::from_utf8_lossy(body).replacen("<html ", "<html class=\"open\" ", 1).into_bytes()
            } else {
                body.to_vec()
            };
            let length = body.len();
            let keeping = if *content_type == FONT { FONT_KEEPING } else { "no-cache" };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {length}\r\nCache-Control: {keeping}\r\nConnection: close\r\n\r\n"
            );
            [head.as_bytes(), &body[..]].concat()
        }
        None => empty_response("404 Not Found"),
    };
    connection.write(&response)
}

/// A response with nothing in it but its status, such as "404 Not Found".
fn empty_response(status: &str) -> Vec<u8> {
    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::on_tailnet;

    #[test]
    fn tailnet_addresses() {
        for address in ["100.64.0.15", "100.127.255.1", "127.0.0.1", "::1", "fd7a:115c:a1e0::f", "::ffff:100.64.0.9"] {
            assert!(on_tailnet(address.parse().unwrap()), "{address}");
        }
        for address in ["192.168.1.20", "100.63.255.255", "100.128.0.1", "10.0.0.1", "fd7a:115c:a1e1::1", "2001:db8::1"]
        {
            assert!(!on_tailnet(address.parse().unwrap()), "{address}");
        }
    }
}
