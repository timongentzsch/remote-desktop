//! The network side: serves the client page over HTTPS and runs one session per WebSocket connection.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io::{self, Read};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rustls::ServerConfig;
use serde_json::Value;

use crate::capture::{self, Settings, Shared};
use crate::input::{Input, KEY_LEFTSHIFT};
use crate::tls::{self, Connection};
use crate::{fail, install, ws};

/// The client, compiled into the binary: path, content type, body.
const PAGES: [(&str, &str, &[u8]); 3] = [
    ("/", "text/html", include_bytes!("../../index.html")),
    ("/manifest.json", "application/manifest+json", include_bytes!("../../manifest.json")),
    ("/icon.svg", "image/svg+xml", include_bytes!("../../icon.svg")),
];

struct Host {
    input: Input,
    /// Empty when the owner chose to allow access without a password.
    password: Vec<u8>,
    /// Replaced once a day, so that a renewed certificate is picked up without a restart.
    tls: Mutex<Arc<ServerConfig>>,
}

pub fn run(address: &str) -> ! {
    let password = fs::read(install::password_path())
        .unwrap_or_else(|_| fail("no password is set; run `remote-desktop password`"))
        .trim_ascii()
        .to_vec();
    let input = Input::open()
        .unwrap_or_else(|error| fail(&format!("cannot create input devices through /dev/uinput ({error}); run `remote-desktop install`")));
    let tls = tls::configuration().unwrap_or_else(|error| fail(&error));
    let listener = TcpListener::bind(address).unwrap_or_else(|error| fail(&format!("cannot listen on {address}: {error}")));
    let host = Arc::new(Host { input, password, tls: Mutex::new(tls) });
    thread::spawn({
        let host = host.clone();
        move || loop {
            thread::sleep(Duration::from_secs(24 * 60 * 60));
            match tls::configuration() {
                Ok(fresh) => *host.tls.lock().unwrap() = fresh,
                Err(error) => eprintln!("remote-desktop: could not reload the certificate: {error}"),
            }
        }
    });
    for socket in listener.incoming().flatten() {
        let host = host.clone();
        thread::spawn(move || {
            let _ = serve(socket, &host); // an error here is the client going away
        });
    }
    unreachable!("the listener never stops accepting")
}

fn serve(socket: TcpStream, host: &Host) -> io::Result<()> {
    let configuration = host.tls.lock().unwrap().clone();
    let connection = Arc::new(Connection::accept(&configuration, socket)?);
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        (&*connection).read_exact(&mut byte)?;
        head.push(byte[0]);
        if head.len() > 8192 {
            return Ok(());
        }
    }
    let request = String::from_utf8_lossy(&head);
    let path = request.split(' ').nth(1).unwrap_or("");
    let key = request.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("sec-websocket-key").then(|| value.trim())
    });
    if let (true, Some(key)) = (path == "/ws", key) {
        let accept = ws::accept_key(key);
        connection.write(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes())?;
        session(&connection, host);
        return Ok(());
    }
    let response = match PAGES.iter().find(|page| page.0 == path) {
        Some((_, content_type, body)) => {
            // The page hides its password field when none is needed.
            let open = path == "/" && host.password.is_empty();
            let body = if open { String::from_utf8_lossy(body).replacen("<html ", "<html class=\"open\" ", 1).into_bytes() } else { body.to_vec() };
            let length = body.len();
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {length}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n");
            [head.as_bytes(), &body[..]].concat()
        }
        None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
    };
    connection.write(&response)
}

/// What is waiting to be sent to one client. It holds at most one piece of video, so a slow link makes the
/// capture skip frames instead of queueing them, while control messages are never stuck behind video.
#[derive(Default)]
struct Outbox {
    pending: Mutex<Pending>,
    changed: Condvar,
}

#[derive(Default)]
struct Pending {
    /// The piece and when it was queued.
    video: Option<(Vec<u8>, Instant)>,
    control: VecDeque<Vec<u8>>,
    closed: bool,
}

impl Outbox {
    /// Waits until the previous piece of video has gone out. False once the session is over.
    fn push_video(&self, piece: Vec<u8>) -> bool {
        let mut pending = self.changed.wait_while(self.pending.lock().unwrap(), |pending| pending.video.is_some() && !pending.closed).unwrap();
        pending.video = Some((piece, Instant::now()));
        self.changed.notify_all();
        !pending.closed
    }

    fn push_control(&self, message: Vec<u8>) {
        self.pending.lock().unwrap().control.push_back(message);
        self.changed.notify_all();
    }

    /// The next thing to send, control messages first, and for video when it was queued. None once the
    /// session is over.
    fn next(&self) -> Option<(Vec<u8>, Option<Instant>)> {
        let mut pending = self.changed.wait_while(self.pending.lock().unwrap(), |pending| pending.video.is_none() && pending.control.is_empty() && !pending.closed).unwrap();
        if pending.closed {
            return None;
        }
        let next = match pending.control.pop_front() {
            Some(message) => (message, None),
            None => pending.video.take().map(|(piece, queued)| (piece, Some(queued)))?,
        };
        self.changed.notify_all();
        Some(next)
    }

    fn close(&self) {
        self.pending.lock().unwrap().closed = true;
        self.changed.notify_all();
    }
}

/// One client: checks its login, streams the screen to it and injects its input until it leaves.
fn session(connection: &Arc<Connection>, host: &Host) {
    let login: Value = ws::read(&mut &**connection).ok().flatten().and_then(|message| serde_json::from_slice(&message).ok()).unwrap_or_default();
    if !host.password.is_empty() && !same(login["password"].as_str().unwrap_or("").as_bytes(), &host.password) {
        thread::sleep(Duration::from_secs(1)); // slow down guessing
        return;
    }
    let choice = |name: &str| login[name].as_str().unwrap_or("");
    let bitrate = &login["bitrate"];
    let settings = Settings {
        kbps: bitrate.as_u64().or_else(|| bitrate.as_str()?.parse().ok()).unwrap_or(20000).clamp(500, 50000) as u32,
        fps: match choice("fps") {
            "30" => 30,
            "15" => 15,
            _ => 60,
        },
        cursor: choice("cursor") == "yes",
        full_chroma: choice("chroma") == "444",
        half_size: choice("size") == "half",
    };
    wake_displays(&host.input);

    let outbox = Arc::new(Outbox::default());
    let shared = Arc::new(Shared::default());
    let writer = thread::spawn({
        let (outbox, connection, shared) = (outbox.clone(), connection.clone(), shared.clone());
        move || {
            // Once a second the client is told how long frames took to encode and to send, for its statistics
            // and so that it can lower the bitrate when sending falls behind.
            let (mut reported, mut frames, mut sending) = (Instant::now(), 0, Duration::ZERO);
            while let Some((bytes, queued)) = outbox.next() {
                if connection.write(&bytes).is_err() {
                    break;
                }
                if let Some(queued) = queued {
                    frames += 1;
                    sending += queued.elapsed();
                }
                if frames > 0 && reported.elapsed() >= Duration::from_secs(1) {
                    let encode = shared.encode_micros.load(Ordering::Relaxed) as f64 / 1000.0;
                    let send = sending.as_secs_f64() * 1000.0 / frames as f64;
                    let statistics = format!(r#"["stats",{encode:.1},{send:.1}]"#);
                    if connection.write(&ws::frame(ws::TEXT, statistics.as_bytes())).is_err() {
                        break;
                    }
                    (reported, frames, sending) = (Instant::now(), 0, Duration::ZERO);
                }
            }
            outbox.close();
            connection.close(); // makes the read loop below finish
        }
    });
    let capture = thread::spawn({
        let (outbox, shared) = (outbox.clone(), shared.clone());
        move || {
            let sink = Box::new({
                let outbox = outbox.clone();
                move |piece: &[u8]| outbox.push_video(ws::frame(ws::BINARY, piece))
            });
            if let Err(reason) = capture::run(settings, shared, sink) {
                eprintln!("remote-desktop: capture ended: {reason}");
            }
            outbox.close();
        }
    });

    let mut held = HashSet::new();
    while let Ok(Some(message)) = ws::read(&mut &**connection) {
        if connection.take_reply_pending() {
            outbox.push_control(Vec::new());
        }
        let Ok(Value::Array(parts)) = serde_json::from_slice(&message) else { continue };
        let number = |index: usize| parts.get(index).and_then(Value::as_i64).unwrap_or(0);
        match parts.first().and_then(Value::as_str) {
            Some("key") => {
                let (code, down) = (number(1) as u16, number(2) != 0);
                if down { held.insert(code) } else { held.remove(&code) };
                host.input.key(code, down);
            }
            Some("move") => host.input.move_to(number(1) as i32, number(2) as i32),
            Some("scroll") => host.input.scroll(number(1) as i32, number(2) as i32),
            Some("bitrate") => shared.kbps.store(number(1).clamp(500, 50000) as u32, Ordering::Relaxed),
            // Echoed so the client can time the round trip.
            Some("ping") => outbox.push_control(ws::frame(ws::TEXT, &message)),
            _ => {}
        }
    }
    shared.stop.store(true, Ordering::Relaxed);
    outbox.close();
    let _ = (capture.join(), writer.join());
    for code in held {
        host.input.key(code, false);
    }
}

/// Compares without stopping at the first difference, so timing does not reveal how much matched.
fn same(offered: &[u8], password: &[u8]) -> bool {
    offered.len() == password.len() && offered.iter().zip(password).fold(0, |difference, (a, b)| difference | (a ^ b)) == 0
}

/// Capture gets no frames from a sleeping display, and a key press wakes it.
fn wake_displays(input: &Input) {
    let asleep = || {
        fs::read_dir("/sys/class/drm").into_iter().flatten().flatten().any(|connector| {
            let read = |name: &str| fs::read_to_string(connector.path().join(name)).unwrap_or_default();
            read("status") == "connected\n" && read("dpms") != "On\n"
        })
    };
    if asleep() {
        input.key(KEY_LEFTSHIFT, true);
        input.key(KEY_LEFTSHIFT, false);
        for _ in 0..30 {
            if !asleep() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_comparison() {
        assert!(same(b"secret", b"secret"));
        assert!(!same(b"secreT", b"secret"));
        assert!(!same(b"secre", b"secret"));
        assert!(!same(b"", b"secret"));
    }

    #[test]
    fn outbox_sends_control_before_video_and_holds_one_piece() {
        let outbox = Arc::new(Outbox::default());
        assert!(outbox.push_video(vec![1]));
        outbox.push_control(vec![2]);
        // A second piece of video has to wait until the first was taken.
        let second = thread::spawn({
            let outbox = outbox.clone();
            move || outbox.push_video(vec![3])
        });
        let next = || outbox.next().map(|(bytes, queued)| (bytes, queued.is_some()));
        assert_eq!(next(), Some((vec![2], false)));
        assert_eq!(next(), Some((vec![1], true)));
        assert!(second.join().unwrap());
        assert_eq!(next(), Some((vec![3], true)));
        outbox.close();
        assert_eq!(next(), None);
        assert!(!outbox.push_video(vec![4]));
    }
}
