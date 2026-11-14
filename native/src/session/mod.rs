//! One client's session: its login, the threads that stream the screen, sound and clipboard to it, and the
//! input it sends back.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::audio::Sound;
use crate::capture::{Output, Shared};
use crate::outbox::{Next, Outbox};
use crate::platform::virtual_screen::{self, VirtualScreen};
use crate::platform::{self, clipboard};
use crate::quic::Link;
use crate::rate::{self, Controller, Reading};
use crate::server::Host;
use crate::tls::Connection;
use crate::{screen, ws};

mod datagrams;
mod login;

use datagrams::Datagrams;
use login::Login;

/// How long a logged-in client may stay silent. Its page measures the round trip several times a second, and
/// still once a minute while the browser has put a hidden tab to sleep, so longer means the client has gone.
const SILENCE_LIMIT: Duration = Duration::from_secs(180);
/// How often the writer looks at the connection when it has nothing to send, and how often during a probe.
const LOOK: Duration = Duration::from_millis(250);
const PROBE_LOOK: Duration = Duration::from_millis(5);
/// Padding goes out in pieces of at most this size, so that whatever comes up meanwhile waits behind little of it.
const PADDING_PIECE: usize = 16 * 1024;
/// How often the writer tells the client how the stream is doing.
const REPORT: Duration = Duration::from_secs(1);
/// The longest round trip a client's report is taken at, in milliseconds.
const LONGEST_ROUND_TRIP: i64 = 60_000;
/// Whatever sound takes of the rate, video is left at least one part in this many.
const MOSTLY_VIDEO: u32 = 2;
/// Where padding's noise begins (see `padding`); any number but zero does.
const NOISE_SEED: u64 = 0x9E37_79B9_7F4A_7C15;
/// What the client's messages tell the session's writer.
struct FromClient {
    /// The highest bitrate it wants, in kbit/s.
    ceiling: AtomicU32,
    /// For how long it would rather have video fall behind than see the picture made coarser, in milliseconds.
    patience: AtomicU32,
    /// The round trip it last measured, in milliseconds; 0 while it has none to report.
    round_trip: AtomicU32,
    /// How many bytes it said had arrived, and when it said so.
    received: Mutex<Option<rate::Mark>>,
}

/// Checks the client's login, then streams to it and injects its input until it leaves.
pub fn run(connection: &Arc<Connection>, host: &Arc<Host>) {
    let login = ws::read(&mut &**connection)
        .ok()
        .flatten()
        .and_then(|message| serde_json::from_slice(&message).ok())
        .unwrap_or_default();
    let login = Login::read(&login);
    if !login::admitted(login.password.as_bytes(), &host.password) {
        return;
    }
    connection.set_patience(SILENCE_LIMIT);
    platform::wake_displays(&host.input);

    // A client that asked to match its device is shown a screen of its window's size, where one can be made;
    // it lasts as long as the session.
    let mut settings = login.settings;
    let made = login.device.and_then(|window| {
        VirtualScreen::create(window, settings.fps)
            .map_err(|error| warn!("showing the machine's own screen: {error}"))
            .ok()
    });
    if made.is_some() {
        settings.screen = Some(virtual_screen::NAME.to_owned());
    }
    // A client that draws the pointer itself must not get the desktop's in the picture as well; it shows
    // again when the session ends, before the screens made for it go.
    let _pointer = (!settings.cursor).then(platform::hide_pointer).flatten();
    // The client is told which screens there are and which it is shown, and its pointer is placed on that one.
    let screens = screen::list();
    let shown = screen::choose(&screens, settings.screen.as_deref()).cloned();
    let outbox = Arc::new(Outbox::default());
    if let Some(shown) = &shown {
        let names: Vec<&str> = screens.iter().map(|screen| screen.name.as_str()).collect();
        outbox.push_control(text(&json!(["screens", names, shown.name])));
    }
    // Where the host has a UDP side, the client is told how to open its end of a link.
    let link = host.udp.get().map(|udp| udp.link());
    let way_by_udp = || {
        let (udp, link) = (host.udp.get()?, link.as_ref()?);
        let hex = |bytes: &[u8]| bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        Some(text(&json!(["udp", udp.port, hex(&udp.fingerprint()), hex(&link.token)])))
    };
    if let Some(message) = way_by_udp() {
        outbox.push_control(message);
    }
    let shared = Arc::new(Shared { patience: login.patience.into(), ..Shared::default() });
    let told = Arc::new(FromClient {
        ceiling: login.ceiling.into(),
        patience: login.patience.into(),
        round_trip: 0.into(),
        received: None.into(),
    });
    let inbox = clipboard::Inbox::new().ok();
    // The session lasts as long as its writer and its capture do. The clipboard and the sound may give up
    // without ending it.
    let mut threads = vec![
        thread::spawn({
            let (outbox, connection, shared, told) = (outbox.clone(), connection.clone(), shared.clone(), told.clone());
            let link = link.clone();
            move || send(&outbox, &connection, &shared, &told, link.as_deref())
        }),
        thread::spawn({
            let (outbox, shared) = (outbox.clone(), shared.clone());
            move || {
                let sink = Box::new({
                    let outbox = outbox.clone();
                    move |output: Output| match output {
                        Output::Video(piece) => outbox.push_video(piece.to_vec()),
                        // A message of its own kind: the shape's size and the place of its tip, then its pixels.
                        Output::Pointer(shape) => {
                            let numbers = [shape.width, shape.height, shape.hot.0, shape.hot.1];
                            let head = numbers.iter().flat_map(|number| number.to_be_bytes());
                            let message: Vec<u8> =
                                [ws::POINTER].into_iter().chain(head).chain(shape.pixels.iter().copied()).collect();
                            outbox.push_control(ws::frame(ws::BINARY, &message));
                            true
                        }
                    }
                });
                if let Err(reason) = platform::capture(settings, shared, sink) {
                    warn!("capture ended: {reason}");
                }
                outbox.close();
            }
        }),
    ];
    // Clipboard text goes both ways where the desktop allows it; the session works without.
    if let Some(inbox) = inbox.clone() {
        let (outbox, shared) = (outbox.clone(), shared.clone());
        threads.push(thread::spawn(move || {
            let copied = Box::new(move |copied: String| {
                outbox.push_control(text(&json!(["clipboard", copied])));
            });
            if let Err(reason) = clipboard::run(&inbox, &shared, copied) {
                warn!("the clipboard is not shared: {reason}");
            }
        }));
    }
    if login.sound {
        let (outbox, shared) = (outbox.clone(), shared.clone());
        threads.push(thread::spawn(move || {
            let packet = Box::new(move |packet: &[u8]| outbox.push_audio([&[ws::SOUND], packet].concat()));
            if let Err(reason) = Sound::new(packet).and_then(|mut sound| platform::record_sound(shared, &mut sound)) {
                warn!("no sound: {reason}");
            }
        }));
    }

    // The pointer's moves come over the connection and, where there is a link, over that as well. Each has its
    // count, and one that is not newer than the last made is left out: whichever way is quicker counts.
    let moved = Arc::new({
        let (host, screens, shown, last) = (host.clone(), screens.clone(), shown.clone(), AtomicU32::new(0));
        move |count: u32, x: i32, y: i32| {
            if count == 0 || last.fetch_max(count, Ordering::Relaxed) < count {
                let (x, y) = shown.as_ref().map_or((x, y), |screen| screen::place(screen, &screens, x, y));
                host.input.move_to(x, y);
            }
        }
    });
    if let Some(link) = &link {
        let moved = moved.clone();
        link.on_move(Box::new(move |count, x, y| moved(count, x.into(), y.into())));
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
            Some("text") => host.input.text(parts.get(1).and_then(Value::as_str).unwrap_or("")),
            Some("move") => moved(number(3) as u32, number(1) as i32, number(2) as i32),
            Some("scroll") => host.input.scroll(number(1) as i32, number(2) as i32),
            Some("bitrate") => {
                told.ceiling.store(login::ceiling(parts.get(1).unwrap_or(&Value::Null)), Ordering::Relaxed)
            }
            Some("patience") => {
                let patience = login::patience(parts.get(1).unwrap_or(&Value::Null));
                told.patience.store(patience, Ordering::Relaxed);
                shared.patience.store(patience, Ordering::Relaxed);
            }
            // The client's window changed size: so does the screen made for it. The pointer needs nothing new, as
            // that screen is then the only one.
            Some("window") => {
                if let (Some(made), Some(window)) = (&made, parts.get(1).and_then(login::window))
                    && let Err(error) = made.resize(window)
                {
                    warn!("the screen keeps its size: {error}");
                }
            }
            // The page asks again how to reach the UDP side when it could not: the certificate there is renewed
            // every day, and the fingerprint it was told may be yesterday's.
            Some("udp") => {
                if let Some(message) = way_by_udp() {
                    outbox.push_control(message);
                }
            }
            Some("clipboard") => {
                if let (Some(inbox), Some(text)) = (&inbox, parts.get(1).and_then(Value::as_str)) {
                    inbox.put(text.to_string());
                }
            }
            // Echoed, with the time the client put into it, so that it can tell the round trip. It also carries
            // the round trip measured last and how many bytes have arrived, which the bitrate is set by.
            Some("decode") => shared.decode_micros.store(number(1).clamp(0, u32::MAX.into()) as u32, Ordering::Relaxed),
            Some("ping") => {
                told.round_trip.store(number(2).clamp(0, LONGEST_ROUND_TRIP) as u32, Ordering::Relaxed);
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
    if let Some(link) = link {
        link.close();
    }
    for code in held {
        host.input.key(code, false);
    }
}

/// The session's writer: sends what the outbox holds until the session is over or the client is gone. It sees
/// how well everything gets through, so it also sets the bitrate (see rate.rs), sends the padding that probing
/// for a higher one takes, and tells the client once a second how the stream is doing. Video goes over the
/// link while the client can be reached that way, and over the connection otherwise.
fn send(outbox: &Outbox, connection: &Connection, shared: &Shared, told: &FromClient, link: Option<&Link>) {
    let mut congestion = connection.congestion();
    let mut controller = Controller::new(Instant::now(), told.ceiling.load(Ordering::Relaxed), 0);
    let (mut written, mut patience, mut noise) = (0, LOOK, NOISE_SEED);
    let (mut reported, mut frames, mut sending, mut send) = (Instant::now(), 0, Duration::ZERO, 0.0);
    let mut datagrams = link.map(Datagrams::new);
    let (mut by_udp, mut kbps) = (false, rate::start(told.ceiling.load(Ordering::Relaxed)));
    // How many bytes of sound went out since the last report, and the rate that made, in kbit/s.
    let (mut spoken, mut sound) = (0, 0);
    let mut surplus = 0.0; // what a probe offers beyond the rate (see `rate::Verdict`)
    loop {
        // On either way the video begins anew: what the other carried last may never have arrived.
        let up = datagrams.as_ref().is_some_and(|datagrams| datagrams.link.is_up());
        let switched = up != by_udp;
        if switched {
            by_udp = up;
            if let Some(datagrams) = &mut datagrams {
                datagrams.begin(Instant::now());
                // The client is told when video is back on the connection, and where over UDP it would go on.
                let back = json!(["path", datagrams.pieces]);
                if !by_udp && connection.write(&text(&back)).is_err() {
                    break;
                }
            }
            shared.fresh.store(true, Ordering::Relaxed);
        }
        // Over UDP nothing holds the writer up as a full connection does. So video waits there while the link
        // has not taken what it was given: the capture skips frames meanwhile, as it does over TCP, and the
        // bitrate goes by that.
        let mut udp = datagrams.as_mut().filter(|_| by_udp); // the link, while video takes it
        let patient = Duration::from_millis(told.patience.load(Ordering::Relaxed).into());
        let room = udp.as_mut().is_none_or(|udp| udp.has_room(Instant::now(), kbps, patient));
        if !room {
            patience = patience.min(PROBE_LOOK);
        }
        let mut left = 0; // frames that leave in this round
        match (outbox.next(patience, room), &mut udp) {
            (Next::Closed, _) => break,
            (Next::Nothing, _) => {}
            (Next::Send(piece, Some(ready)), Some(udp)) => {
                udp.piece(&piece, kbps, surplus);
                (left, sending) = (1, sending + ready.elapsed());
            }
            (Next::Sound(packet), Some(udp)) => {
                let before = udp.written;
                udp.sound(&packet, kbps);
                spoken += udp.written - before;
            }
            (Next::Sound(packet), None) => {
                let message = ws::frame(ws::BINARY, &packet);
                if connection.write(&message).is_err() {
                    break;
                }
                written += message.len() as u64;
                spoken += message.len() as u64;
            }
            (Next::Send(bytes, ready), _) => {
                // A piece of video is yet to be made a message; everything else comes as one.
                let message = if ready.is_some() { ws::frame(ws::BINARY, &bytes) } else { bytes };
                if connection.write(&message).is_err() {
                    break;
                }
                written += message.len() as u64;
                if let Some(ready) = ready {
                    (left, sending) = (1, sending + ready.elapsed());
                }
            }
        }
        frames += left;
        let now = Instant::now();
        congestion.review(now);
        let (coarse, held_up) = (shared.coarse.swap(0, Ordering::Relaxed), outbox.take_held_up());
        if let Some(udp) = &mut udp {
            udp.measure(now);
        }
        let ceiling = told.ceiling.load(Ordering::Relaxed);
        // Over UDP the connection itself says what arrived and how long the way is; over TCP the client does.
        let fared = udp.as_ref().and_then(|udp| Some((udp.written, udp.link.fared(now)?)));
        let (sent, received, round_trip) = match fared {
            Some((sent, fared)) => (sent, Some(fared.arrived), Some(fared.round_trip)),
            None => {
                let round_trip = Duration::from_millis(told.round_trip.load(Ordering::Relaxed).into());
                (written, *told.received.lock().unwrap(), Some(round_trip))
            }
        };
        let round_trip = round_trip.filter(|round_trip| !round_trip.is_zero());
        let lost = udp.as_ref().map(|udp| udp.lately);
        let reading = Reading {
            now,
            written: sent,
            received,
            round_trip,
            lost,
            frames: left,
            coarse,
            held_up,
            ceiling,
            patience: patient,
        };
        if switched {
            // What was measured on the way so far says nothing about this one: the rate is found anew.
            controller = Controller::new(now, ceiling, sent);
        }
        let verdict = controller.step(&reading);
        (kbps, surplus) = (verdict.kbps, verdict.surplus);
        // The rate is what the link is given in all. Sound takes its part of that, which on a slow link is
        // no small one, though never most; and over UDP the spare datagrams take their share of the rest.
        let share = udp.as_ref().map_or(1.0, |udp| udp.share());
        let video = kbps.saturating_sub(sound).max(kbps / MOSTLY_VIDEO);
        shared.kbps.store((video as f64 * share) as u32, Ordering::Relaxed);
        shared.climbing.store(verdict.climbing, Ordering::Relaxed);
        shared.held_back.store(verdict.held_back, Ordering::Relaxed);
        if let Some(udp) = &mut udp
            && udp.needs_fresh(now, reading.round_trip.unwrap_or_default())
        {
            shared.fresh.store(true, Ordering::Relaxed);
        }
        patience = if verdict.probing { PROBE_LOOK } else { LOOK };
        if reported.elapsed() >= REPORT {
            sound = (spoken as f64 * 8.0 / 1000.0 / reported.elapsed().as_secs_f64()) as u32;
            spoken = 0;
            let encode = shared.encode_micros.load(Ordering::Relaxed) as f64 / 1000.0;
            if frames > 0 {
                send = sending.as_secs_f64() * 1000.0 / frames as f64;
            }
            let tenths = |milliseconds: f64| (milliseconds * 10.0).round() / 10.0;
            outbox.push_control(text(&json!(["stats", tenths(encode), tenths(send), verdict.kbps, verdict.held_back])));
            (reported, frames, sending) = (Instant::now(), 0, Duration::ZERO);
        }
        // Padding waits for room as video does, or it would keep the link full and the video out.
        if verdict.padding > 0 && room {
            let due = verdict.padding.min(PADDING_PIECE);
            match &mut udp {
                Some(udp) => {
                    let size = udp.link.room();
                    let pieces = (0..due.div_ceil(size)).map(|_| {
                        let mut piece = padding(size, &mut noise);
                        piece.truncate(size);
                        piece
                    });
                    udp.send(pieces.collect(), kbps);
                }
                None => {
                    let piece = ws::frame(ws::BINARY, &padding(due, &mut noise));
                    if connection.write(&piece).is_err() {
                        break;
                    }
                    written += piece.len() as u64;
                }
            }
            patience = Duration::ZERO; // more may be due, but what the outbox holds by now goes first
        }
    }
    outbox.close();
    connection.close(); // makes the session's read loop finish
}

/// A text message to the client: a JSON array whose first element names it.
fn text(message: &Value) -> Vec<u8> {
    ws::frame(ws::TEXT, message.to_string().as_bytes())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_is_marked_and_does_not_repeat() {
        let mut noise = 1;
        let (first, second) = (padding(4096, &mut noise), padding(4096, &mut noise));
        assert_eq!(first[0], ws::PADDING);
        assert!((4096..4104).contains(&first.len()));
        assert_ne!(first, second);
    }
}
