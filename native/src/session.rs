//! One client's session: its login, the threads that stream the screen, sound and clipboard to it, and the
//! input it sends back.

use std::collections::HashSet;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::capture::{self, Settings, Shared};
use crate::input::{Input, KEY_LEFTSHIFT};
use crate::outbox::Outbox;
use crate::server::Host;
use crate::tls::Connection;
use crate::{audio, clipboard, ws};

/// Checks the client's login, then streams to it and injects its input until it leaves.
pub fn run(connection: &Arc<Connection>, host: &Host) {
    let login: Value = ws::read(&mut &**connection).ok().flatten().and_then(|message| serde_json::from_slice(&message).ok()).unwrap_or_default();
    if !host.password.is_empty() && !same(login["password"].as_str().unwrap_or("").as_bytes(), &host.password) {
        thread::sleep(Duration::from_secs(1)); // slow down guessing
        return;
    }
    wake_displays(&host.input);

    let outbox = Arc::new(Outbox::default());
    let shared = Arc::new(Shared::default());
    let inbox = clipboard::Inbox::new().ok();
    // The session lasts as long as its writer and its capture do. The clipboard and the sound may give up
    // without ending it.
    let mut threads = vec![
        thread::spawn({
            let (outbox, connection, shared) = (outbox.clone(), connection.clone(), shared.clone());
            move || send(&outbox, &connection, &shared)
        }),
        thread::spawn({
            let (outbox, shared, settings) = (outbox.clone(), shared.clone(), settings(&login));
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
        }),
    ];
    // Clipboard text goes both ways where the compositor allows it; the session works without.
    if let Some(inbox) = inbox.clone() {
        let (outbox, shared) = (outbox.clone(), shared.clone());
        threads.push(thread::spawn(move || {
            let copied = Box::new(move |text: String| {
                outbox.push_control(ws::frame(ws::TEXT, serde_json::json!(["clipboard", text]).to_string().as_bytes()));
            });
            if let Err(reason) = clipboard::run(inbox, shared, copied) {
                eprintln!("remote-desktop: the clipboard is not shared: {reason}");
            }
        }));
    }
    if login["sound"] == "on" {
        let (outbox, shared) = (outbox.clone(), shared.clone());
        threads.push(thread::spawn(move || {
            // Sound is its own kind of binary message: it starts with 8, where video starts with 9 or the stream header.
            let packet = Box::new(move |packet: &[u8]| outbox.push_audio(ws::frame(ws::BINARY, &[&[8], packet].concat())));
            if let Err(reason) = audio::run(shared, packet) {
                eprintln!("remote-desktop: no sound: {reason}");
            }
        }));
    }

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
            Some("bitrate") => shared.kbps.store(kbps(number(1) as u64), Ordering::Relaxed),
            Some("clipboard") => {
                if let (Some(inbox), Some(text)) = (&inbox, parts.get(1).and_then(Value::as_str)) {
                    inbox.put(text.to_string());
                }
            }
            // Echoed so the client can time the round trip.
            Some("ping") => outbox.push_control(ws::frame(ws::TEXT, &message)),
            _ => {}
        }
    }
    shared.stop.store(true, Ordering::Relaxed);
    outbox.close();
    for thread in threads {
        let _ = thread.join();
    }
    for code in held {
        host.input.key(code, false);
    }
}

/// What the client chose on its settings panel; anything missing or unknown means the default.
fn settings(login: &Value) -> Settings {
    let bitrate = &login["bitrate"];
    Settings {
        kbps: kbps(bitrate.as_u64().or_else(|| bitrate.as_str()?.parse().ok()).unwrap_or(20000)),
        fps: match login["fps"].as_str() {
            Some("30") => 30,
            Some("15") => 15,
            _ => 60,
        },
        cursor: login["cursor"] == "yes",
        full_chroma: login["chroma"] == "444",
        half_size: login["size"] == "half",
    }
}

/// A bitrate cap the client asked for, held to what makes sense.
fn kbps(wanted: u64) -> u32 {
    wanted.clamp(500, 50000) as u32
}

/// The session's writer: sends what the outbox holds until the session is over or the client is gone. Once a
/// second it tells the client how long frames took to encode and to send, for its statistics and so that it
/// can lower the bitrate when sending falls behind.
fn send(outbox: &Outbox, connection: &Connection, shared: &Shared) {
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
    connection.close(); // makes the session's read loop finish
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
}
