//! The network side: serves the client page and runs one session per WebSocket connection.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::capture::{self, Settings};
use crate::input::{Input, KEY_LEFTSHIFT};
use crate::{fail, install, ws};

/// The client, compiled into the binary: path, content type, body.
const PAGES: [(&str, &str, &[u8]); 3] = [
    ("/", "text/html", include_bytes!("../../index.html")),
    ("/manifest.json", "application/manifest+json", include_bytes!("../../manifest.json")),
    ("/icon.svg", "image/svg+xml", include_bytes!("../../icon.svg")),
];

pub fn run(address: &str) -> ! {
    let password = fs::read(install::password_path()).unwrap_or_default().trim_ascii().to_vec();
    if password.is_empty() {
        fail("no password is set; run `remote-desktop password`");
    }
    let input = Input::open()
        .unwrap_or_else(|error| fail(&format!("cannot create input devices through /dev/uinput ({error}); run `remote-desktop install`")));
    let listener = TcpListener::bind(address).unwrap_or_else(|error| fail(&format!("cannot listen on {address}: {error}")));
    let shared = Arc::new((input, password));
    for stream in listener.incoming().flatten() {
        let shared = shared.clone();
        thread::spawn(move || {
            let _ = serve(stream, &shared.0, &shared.1); // an error here is the client going away
        });
    }
    unreachable!("the listener never stops accepting")
}

fn serve(mut stream: TcpStream, input: &Input, password: &[u8]) -> io::Result<()> {
    stream.set_nodelay(true)?;
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
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
        write!(stream, "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n")?;
        return session(stream, input, password);
    }
    match PAGES.iter().find(|page| page.0 == path) {
        Some((_, content_type, body)) => {
            let length = body.len();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {length}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n")?;
            stream.write_all(body)
        }
        None => stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
    }
}

/// One client: checks its login, streams the screen to it and injects its input until it leaves.
fn session(mut stream: TcpStream, input: &Input, password: &[u8]) -> io::Result<()> {
    let login: Value = ws::read(&mut stream)?.and_then(|message| serde_json::from_slice(&message).ok()).unwrap_or_default();
    if !same(login["password"].as_str().unwrap_or("").as_bytes(), password) {
        thread::sleep(Duration::from_secs(1)); // slow down guessing
        return Ok(());
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
    wake_displays(input);

    let writer = Arc::new(Mutex::new(stream.try_clone()?));
    let stop = Arc::new(AtomicBool::new(false));
    let capture = thread::spawn({
        let (writer, stop, connection) = (writer.clone(), stop.clone(), stream.try_clone()?);
        move || {
            let sink = Box::new(move |piece: &[u8]| ws::write(&mut *writer.lock().unwrap(), ws::BINARY, piece).is_ok());
            if let Err(reason) = capture::run(settings, stop, sink) {
                eprintln!("remote-desktop: capture ended: {reason}");
            }
            let _ = connection.shutdown(Shutdown::Both); // makes the read loop below finish
        }
    });

    let mut held = HashSet::new();
    while let Ok(Some(message)) = ws::read(&mut stream) {
        let Ok(Value::Array(parts)) = serde_json::from_slice(&message) else { continue };
        let number = |index: usize| parts.get(index).and_then(Value::as_i64).unwrap_or(0);
        match parts.first().and_then(Value::as_str) {
            Some("key") => {
                let (code, down) = (number(1) as u16, number(2) != 0);
                if down { held.insert(code) } else { held.remove(&code) };
                input.key(code, down);
            }
            Some("move") => input.move_to(number(1) as i32, number(2) as i32),
            Some("scroll") => input.scroll(number(1) as i32, number(2) as i32),
            // Echoed so the client can time the round trip.
            Some("ping") => drop(ws::write(&mut *writer.lock().unwrap(), ws::TEXT, &message)),
            _ => {}
        }
    }
    stop.store(true, Ordering::Relaxed);
    let _ = capture.join();
    for code in held {
        input.key(code, false);
    }
    Ok(())
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
    use super::same;

    #[test]
    fn password_comparison() {
        assert!(same(b"secret", b"secret"));
        assert!(!same(b"secreT", b"secret"));
        assert!(!same(b"secre", b"secret"));
        assert!(!same(b"", b"secret"));
    }
}
