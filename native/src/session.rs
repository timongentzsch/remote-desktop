//! One client's session: its login, the threads that stream the screen, sound and clipboard to it, and the
//! input it sends back.

use std::collections::HashSet;
use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::capture::{self, Settings, Shared};
use crate::input::{Input, KEY_LEFTSHIFT};
use crate::outbox::{Next, Outbox};
use crate::rate::{self, Controller, Reading};
use crate::server::Host;
use crate::tls::Connection;
use crate::{audio, clipboard, ws};

/// How long a logged-in client may stay silent. Its page measures the round trip several times a second, and
/// still once a minute while the browser has put a hidden tab to sleep, so longer means the client has gone.
const SILENCE_LIMIT: Duration = Duration::from_secs(180);
/// How long after a wrong password no login is accepted, which limits guessing to this pace overall.
const GUESS_INTERVAL: Duration = Duration::from_secs(1);
/// How often the writer looks at the connection when it has nothing to send, and how often during a probe.
const LOOK: Duration = Duration::from_millis(250);
const PROBE_LOOK: Duration = Duration::from_millis(5);
/// Padding goes out in pieces of at most this size, so that whatever comes up meanwhile waits behind little of it.
const PADDING_PIECE: usize = 16 * 1024;

/// What the client's messages tell the session's writer.
struct FromClient {
    /// The highest bitrate it wants, in kbit/s.
    ceiling: AtomicU32,
    /// The round trip it last measured, in milliseconds; 0 while it has none to report.
    round_trip: AtomicU32,
    /// How many bytes it said had arrived, and when it said so.
    received: Mutex<Option<rate::Mark>>,
}

/// Checks the client's login, then streams to it and injects its input until it leaves.
pub fn run(connection: &Arc<Connection>, host: &Host) {
    let login: Value = ws::read(&mut &**connection)
        .ok()
        .flatten()
        .and_then(|message| serde_json::from_slice(&message).ok())
        .unwrap_or_default();
    if !admitted(login["password"].as_str().unwrap_or("").as_bytes(), &host.password) {
        return;
    }
    connection.set_patience(SILENCE_LIMIT);
    wake_displays(&host.input);

    let outbox = Arc::new(Outbox::default());
    let shared = Arc::new(Shared::default());
    let told = Arc::new(FromClient {
        ceiling: ceiling(&login["bitrate"]).into(),
        round_trip: 0.into(),
        received: None.into(),
    });
    let inbox = clipboard::Inbox::new().ok();
    // The session lasts as long as its writer and its capture do. The clipboard and the sound may give up
    // without ending it.
    let mut threads = vec![
        thread::spawn({
            let (outbox, connection, shared, told) = (outbox.clone(), connection.clone(), shared.clone(), told.clone());
            move || send(&outbox, &connection, &shared, &told)
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
    // Clipboard text goes both ways where the desktop allows it; the session works without.
    if let Some(inbox) = inbox.clone() {
        let (outbox, shared) = (outbox.clone(), shared.clone());
        threads.push(thread::spawn(move || {
            let copied = Box::new(move |text: String| {
                outbox.push_control(ws::frame(ws::TEXT, serde_json::json!(["clipboard", text]).to_string().as_bytes()));
            });
            if let Err(reason) = clipboard::run(&inbox, &shared, copied) {
                eprintln!("remote-desktop: the clipboard is not shared: {reason}");
            }
        }));
    }
    if login["sound"] == "on" {
        let (outbox, shared) = (outbox.clone(), shared.clone());
        threads.push(thread::spawn(move || {
            let packet = Box::new(move |packet: &[u8]| {
                outbox.push_audio(ws::frame(ws::BINARY, &[&[ws::SOUND], packet].concat()))
            });
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
                if down {
                    held.insert(code)
                } else {
                    held.remove(&code)
                };
                host.input.key(code, down);
            }
            Some("move") => host.input.move_to(number(1) as i32, number(2) as i32),
            Some("scroll") => host.input.scroll(number(1) as i32, number(2) as i32),
            Some("bitrate") => told.ceiling.store(ceiling(parts.get(1).unwrap_or(&Value::Null)), Ordering::Relaxed),
            Some("clipboard") => {
                if let (Some(inbox), Some(text)) = (&inbox, parts.get(1).and_then(Value::as_str)) {
                    inbox.put(text.to_string());
                }
            }
            // Echoed, with the time the client put into it, so that it can tell the round trip. It also carries
            // the round trip measured last and how many bytes have arrived, which the bitrate is set by.
            Some("ping") => {
                told.round_trip.store(number(2).clamp(0, 60_000) as u32, Ordering::Relaxed);
                if number(3) > 0 {
                    *told.received.lock().unwrap() = Some((Instant::now(), number(3) as u64));
                }
                outbox.push_control(ws::frame(ws::TEXT, &message));
            }
            _ => {}
        }
    }
    shared.stop.store(true, Ordering::Relaxed);
    outbox.close();
    connection.close(); // frees the writer if it is stuck sending to a client that no longer listens
    for thread in threads {
        let _ = thread.join();
    }
    for code in held {
        host.input.key(code, false);
    }
}

/// What the client chose on its settings panel; anything missing or unknown means the default.
fn settings(login: &Value) -> Settings {
    Settings {
        kbps: rate::start(ceiling(&login["bitrate"])),
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

/// The highest bitrate a client asks for, in kbit/s. Nothing or zero leaves the choice to the host.
fn ceiling(wanted: &Value) -> u32 {
    match wanted.as_i64().or_else(|| wanted.as_str()?.parse().ok()) {
        Some(kbps) if kbps > 0 => kbps.clamp(rate::LOWEST.into(), 50000) as u32,
        _ => rate::HIGHEST,
    }
}

/// The session's writer: sends what the outbox holds until the session is over or the client is gone. It sees
/// how well everything gets through, so it also sets the bitrate (see rate.rs), sends the padding that probing
/// for a higher one takes, and tells the client once a second how the stream is doing.
fn send(outbox: &Outbox, connection: &Connection, shared: &Shared, told: &FromClient) {
    let mut controller = Controller::new(Instant::now(), told.ceiling.load(Ordering::Relaxed));
    let (mut written, mut patience, mut noise) = (0, LOOK, 0x9E37_79B9_7F4A_7C15);
    let (mut reported, mut frames, mut sending, mut send) = (Instant::now(), 0, Duration::ZERO, 0.0);
    loop {
        let mut left = 0; // frames that leave in this round
        match outbox.next(patience) {
            Next::Closed => break,
            Next::Nothing => {}
            Next::Send(bytes, ready) => {
                if connection.write(&bytes).is_err() {
                    break;
                }
                written += bytes.len() as u64;
                if let Some(ready) = ready {
                    left = 1;
                    sending += ready.elapsed();
                }
            }
        }
        frames += left;
        let round_trip = told.round_trip.load(Ordering::Relaxed);
        let verdict = controller.step(&Reading {
            now: Instant::now(),
            written,
            received: *told.received.lock().unwrap(),
            round_trip: (round_trip > 0).then(|| Duration::from_millis(round_trip.into())),
            frames: left,
            held_up: outbox.take_held_up(),
            ceiling: told.ceiling.load(Ordering::Relaxed),
        });
        shared.kbps.store(verdict.kbps, Ordering::Relaxed);
        patience = if verdict.probing { PROBE_LOOK } else { LOOK };
        if reported.elapsed() >= Duration::from_secs(1) {
            let encode = shared.encode_micros.load(Ordering::Relaxed) as f64 / 1000.0;
            if frames > 0 {
                send = sending.as_secs_f64() * 1000.0 / frames as f64;
            }
            let statistics = format!(r#"["stats",{encode:.1},{send:.1},{},{}]"#, verdict.kbps, verdict.held_back);
            outbox.push_control(ws::frame(ws::TEXT, statistics.as_bytes()));
            (reported, frames, sending) = (Instant::now(), 0, Duration::ZERO);
        }
        if verdict.padding > 0 {
            let piece = ws::frame(ws::BINARY, &padding(verdict.padding.min(PADDING_PIECE), &mut noise));
            if connection.write(&piece).is_err() {
                break;
            }
            written += piece.len() as u64;
            patience = Duration::ZERO; // more may be due, but what the outbox holds by now goes first
        }
    }
    outbox.close();
    connection.close(); // makes the session's read loop finish
}

/// A padding message of about `length` bytes. They are noise: a tunnel that compresses cannot shrink them and
/// so make the link look faster than video will find it.
fn padding(length: usize, noise: &mut u64) -> Vec<u8> {
    let mut message = vec![ws::PADDING];
    while message.len() < length {
        *noise ^= *noise << 13;
        *noise ^= *noise >> 7;
        *noise ^= *noise << 17;
        message.extend(noise.to_le_bytes());
    }
    message
}

/// Whether a login with this password is let in. After a wrong one, every login is turned away for a moment,
/// the right password included, so that many connections guessing at once gain nothing.
fn admitted(offered: &[u8], password: &[u8]) -> bool {
    static LAST_WRONG: Mutex<Option<Instant>> = Mutex::new(None);
    if password.is_empty() {
        return true;
    }
    let mut last_wrong = LAST_WRONG.lock().unwrap();
    if last_wrong.is_some_and(|at| at.elapsed() < GUESS_INTERVAL) {
        return false;
    }
    let right = same(offered, password);
    if !right {
        *last_wrong = Some(Instant::now());
    }
    right
}

/// Compares without stopping at the first difference, so timing does not reveal how much matched.
fn same(offered: &[u8], password: &[u8]) -> bool {
    offered.len() == password.len()
        && offered.iter().zip(password).fold(0, |difference, (a, b)| difference | (a ^ b)) == 0
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
    fn a_wrong_password_closes_the_door_for_a_moment() {
        assert!(admitted(b"", b""), "no password set: everyone is let in");
        assert!(admitted(b"secret", b"secret"));
        assert!(!admitted(b"guess", b"secret"));
        assert!(!admitted(b"secret", b"secret"), "right after a wrong guess even the right password waits");
        thread::sleep(GUESS_INTERVAL);
        assert!(admitted(b"secret", b"secret"));
    }

    #[test]
    fn the_ceiling_is_the_clients_choice_within_limits() {
        assert_eq!(ceiling(&serde_json::json!(5000)), 5000);
        assert_eq!(ceiling(&serde_json::json!("5000")), 5000);
        assert_eq!(ceiling(&serde_json::json!(1)), rate::LOWEST);
        assert_eq!(ceiling(&serde_json::json!(900_000)), 50000);
        for unset in [serde_json::json!(""), serde_json::json!(0), Value::Null] {
            assert_eq!(ceiling(&unset), rate::HIGHEST);
        }
    }

    #[test]
    fn padding_is_marked_and_does_not_repeat() {
        let mut noise = 1;
        let (first, second) = (padding(4096, &mut noise), padding(4096, &mut noise));
        assert_eq!(first[0], ws::PADDING);
        assert!((4096..4104).contains(&first.len()));
        assert_ne!(first, second);
    }

    #[test]
    fn password_comparison() {
        assert!(same(b"secret", b"secret"));
        assert!(!same(b"secreT", b"secret"));
        assert!(!same(b"secre", b"secret"));
        assert!(!same(b"", b"secret"));
    }
}
