//! One client's session: its login, the threads that stream the screen, sound and clipboard to it, and the
//! input it sends back.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::capture::{self, Output, Settings, Shared};
use crate::input::{Input, KEY_LEFTSHIFT};
use crate::outbox::{Next, Outbox};
use crate::quic::{self, Link};
use crate::rate::{self, Controller, Reading};
use crate::server::Host;
use crate::tls::Connection;
use crate::{audio, clipboard, fec, screen, ws};

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
/// Over UDP a piece of video gets spare datagrams for what the link loses (see fec.rs). The link is taken to
/// lose at least this share, however little it lost lately: loss comes in runs.
const LEAST_LOSS: f64 = 0.01;
/// Over UDP the link has room for more when all that it was given up to a round trip and the client's patience
/// ago has arrived or is given up as lost. The round trip is the shortest of late: one that a queue has made
/// longer would make room for that queue. What the rate sends in this many milliseconds may be older and
/// still under way: packets that are lost count as under way until the connection gives them up, which it
/// takes its time over.
const STRAGGLERS: u64 = 30;
/// On a way whose round trip swings by itself, the patience is at least this many times the usual swing: a
/// round trip of 45 to 210 ms, as a relay showed, would else look like a full link most of the time.
const SWINGS: u32 = 4;
/// How long a client that is behind may stand still, a round trip apart, before it is given a frame to begin
/// with.
const STUCK: Duration = Duration::from_millis(300);

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

    // The client is told which screens there are and which it is shown, and its pointer is placed on that one.
    let (settings, screens) = (settings(&login), screen::list());
    let shown = screen::choose(&screens, settings.screen.as_deref()).cloned();
    let outbox = Arc::new(Outbox::default());
    if let Some(shown) = &shown {
        let names: Vec<&str> = screens.iter().map(|screen| screen.name.as_str()).collect();
        let message = serde_json::json!(["screens", names, shown.name]).to_string();
        outbox.push_control(ws::frame(ws::TEXT, message.as_bytes()));
    }
    // Where the host has a UDP side, the client is told how to open its end of a link.
    let link = host.udp.get().map(|udp| udp.link());
    let way_by_udp = || {
        let (udp, link) = (host.udp.get()?, link.as_ref()?);
        let hex = |bytes: &[u8]| bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let message = serde_json::json!(["udp", udp.port, hex(&udp.fingerprint()), hex(&link.token)]).to_string();
        Some(ws::frame(ws::TEXT, message.as_bytes()))
    };
    if let Some(message) = way_by_udp() {
        outbox.push_control(message);
    }
    let shared = Arc::new(Shared::default());
    let told = Arc::new(FromClient {
        ceiling: ceiling(&login["bitrate"]).into(),
        patience: patience(&login["patience"]).into(),
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
            let packet = Box::new(move |packet: &[u8]| outbox.push_audio([&[ws::SOUND], packet].concat()));
            if let Err(reason) = audio::run(shared, packet) {
                eprintln!("remote-desktop: no sound: {reason}");
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
            Some("move") => moved(number(3) as u32, number(1) as i32, number(2) as i32),
            Some("scroll") => host.input.scroll(number(1) as i32, number(2) as i32),
            Some("bitrate") => told.ceiling.store(ceiling(parts.get(1).unwrap_or(&Value::Null)), Ordering::Relaxed),
            Some("patience") => told.patience.store(patience(parts.get(1).unwrap_or(&Value::Null)), Ordering::Relaxed),
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
    if let Some(link) = link {
        link.close();
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
        screen: login["screen"].as_str().filter(|name| !name.is_empty()).map(str::to_owned),
    }
}

/// For how long a client would rather have video fall behind than see the picture made coarser, in
/// milliseconds: from response first to quality first. Nothing means the middle.
fn patience(wanted: &Value) -> u32 {
    wanted.as_i64().or_else(|| wanted.as_str()?.parse().ok()).map_or(100, |patience| patience.clamp(25, 400) as u32)
}

/// The highest bitrate a client asks for, in kbit/s. Nothing or zero leaves the choice to the host.
fn ceiling(wanted: &Value) -> u32 {
    match wanted.as_i64().or_else(|| wanted.as_str()?.parse().ok()) {
        Some(kbps) if kbps > 0 => kbps.clamp(rate::LOWEST.into(), 50000) as u32,
        _ => rate::HIGHEST,
    }
}

/// The session's video on its way over UDP.
struct Datagrams<'a> {
    link: &'a Link,
    /// How many bytes were handed to the link, and the number of the next piece.
    written: u64,
    pieces: u32,
    /// Of the pieces sent lately, how many bytes they had and how many their datagrams took, spares included.
    plain: f64,
    cut: f64,
    /// The share of what was sent that the link lost lately, that share in the last second alone, and the
    /// connection's counts when that was last worked out.
    loss: f64,
    lately: f64,
    counted: (Instant, u64, u64),
    /// The piece from which on the frame to begin with that was last asked for makes good what is lost.
    fresh_from: u32,
    /// The number of the next packet of sound, and the packet before it, which goes along once more.
    spoken: (u32, Vec<u8>),
    /// What the client last said of the pieces it has, and since when it should have said something else.
    status: (u32, u32),
    since: Instant,
    /// How much the link had been given in all at moments in the last round trip or so.
    given: VecDeque<(Instant, u64)>,
}

impl<'a> Datagrams<'a> {
    fn new(link: &'a Link) -> Self {
        let now = Instant::now();
        Self {
            link,
            written: 0,
            pieces: 0,
            plain: 1.0,
            cut: 1.0,
            loss: 0.0,
            lately: 0.0,
            counted: (now, 0, 0),
            fresh_from: 0,
            spoken: (0, Vec::new()),
            status: link.status(),
            since: now,
            given: VecDeque::new(),
        }
    }

    /// Video changes its way. What the link counted so far, perhaps while nothing got through, is left behind,
    /// and a frame to begin with is on its way.
    fn begin(&mut self, now: Instant) {
        (self.fresh_from, self.since, self.loss, self.lately) = (self.pieces, now, 0.0, 0.0);
        self.given.clear();
        if let Some(fared) = self.link.fared(now) {
            self.counted = (now, fared.sent, fared.lost);
        }
    }

    fn send(&mut self, datagrams: Vec<Vec<u8>>, kbps: u32) {
        self.written += datagrams.iter().map(|datagram| datagram.len() as u64).sum::<u64>();
        self.link.send(datagrams, kbps);
    }

    fn piece(&mut self, piece: &[u8], kbps: u32) {
        if !self.behind() {
            self.since = Instant::now(); // from here on the client has something to catch up with
        }
        let datagrams = fec::cut(self.pieces, piece, self.link.room(), self.loss.max(LEAST_LOSS));
        let took: usize = datagrams.iter().map(Vec::len).sum();
        (self.plain, self.cut) = (0.95 * self.plain + piece.len() as f64, 0.95 * self.cut + took as f64);
        self.send(datagrams, kbps);
        self.pieces += 1;
    }

    /// Sends a packet of sound (a message as the connection carries it: the kind, then the packet) ahead of
    /// what waits, and the packet before it once more: one datagram lost then costs nothing. A datagram is the
    /// kind, the packet's number, its length, the packet, and the one before.
    fn sound(&mut self, message: &[u8], kbps: u32) {
        let (kind, packet) = (message[0], &message[1..]);
        let (number, before) = &mut self.spoken;
        let head = [&[kind][..], &number.to_be_bytes(), &(packet.len() as u16).to_be_bytes()].concat();
        let datagram = [&head[..], packet, before].concat();
        (*number, *before) = (*number + 1, packet.to_vec());
        self.written += datagram.len() as u64;
        self.link.send_ahead(datagram, kbps);
    }

    /// The share of the rate that is left for video itself, beside its spares and what datagrams begin with.
    fn share(&self) -> f64 {
        self.plain / self.cut
    }

    /// Works out anew, once a second, how much the link loses. A rise counts at once; it is forgotten slowly.
    fn measure(&mut self, now: Instant) {
        let (at, sent, lost) = self.counted;
        if now.duration_since(at) >= Duration::from_secs(1)
            && let Some(fared) = self.link.fared(now)
        {
            // Another connection may have taken the link, whose counts begin anew.
            let (more_sent, more_lost) = (fared.sent.saturating_sub(sent), fared.lost.saturating_sub(lost));
            let lately = more_lost as f64 / (more_sent as f64).max(1.0);
            (self.loss, self.lately) = (lately.max(0.8 * self.loss), lately);
            self.counted = (now, fared.sent, fared.lost);
        }
    }

    /// Whether the link has taken what it was given, but for the latest and for stragglers.
    fn has_room(&mut self, now: Instant, kbps: u32, patience: Duration) -> bool {
        let Some(fared) = self.link.fared(now) else { return true };
        self.given.push_back((now, self.written));
        // Of the moments noted, the latest that is long enough ago says what should be done with by now.
        let long_ago = |at: Instant| now.duration_since(at) >= fared.shortest + patience.max(SWINGS * fared.swing);
        while self.given.get(1).is_some_and(|&(at, _)| long_ago(at)) {
            self.given.pop_front();
        }
        let done = self.written.saturating_sub(fared.under_way) + kbps as u64 * 125 * STRAGGLERS / 1000;
        self.given.front().is_none_or(|&(at, given)| !long_ago(at) || done >= given)
    }

    /// Whether the client has not got all that was sent.
    fn behind(&self) -> bool {
        let (used, missing) = self.status;
        missing != quic::NOTHING || (self.pieces > 0 && used != self.pieces - 1)
    }

    /// Whether the client needs a frame to begin with that has not been asked for yet: it says a piece is
    /// missing, or it is behind and has stood still for too long. That is how it shows when the latest piece
    /// got lost, after which nothing comes that would show the gap, or the frame to begin with itself.
    fn needs_fresh(&mut self, now: Instant, round_trip: Duration) -> bool {
        let status = self.link.status();
        if status != self.status {
            (self.status, self.since) = (status, now);
        }
        let gap = status.1 != quic::NOTHING && status.1 >= self.fresh_from;
        let stuck = self.behind() && now.duration_since(self.since) > STUCK + round_trip;
        if gap || stuck {
            (self.fresh_from, self.since) = (self.pieces, now);
        }
        gap || stuck
    }
}

/// The session's writer: sends what the outbox holds until the session is over or the client is gone. It sees
/// how well everything gets through, so it also sets the bitrate (see rate.rs), sends the padding that probing
/// for a higher one takes, and tells the client once a second how the stream is doing. Video goes over the
/// link while the client can be reached that way, and over the connection otherwise.
fn send(outbox: &Outbox, connection: &Connection, shared: &Shared, told: &FromClient, link: Option<&Link>) {
    let mut congestion = connection.congestion();
    let mut controller = Controller::new(Instant::now(), told.ceiling.load(Ordering::Relaxed), 0);
    let (mut written, mut patience, mut noise) = (0, LOOK, 0x9E37_79B9_7F4A_7C15);
    let (mut reported, mut frames, mut sending, mut send) = (Instant::now(), 0, Duration::ZERO, 0.0);
    let mut datagrams = link.map(Datagrams::new);
    let (mut by_udp, mut kbps) = (false, rate::start(told.ceiling.load(Ordering::Relaxed)));
    loop {
        // On either way the video begins anew: what the other carried last may never have arrived.
        let up = datagrams.as_ref().is_some_and(|datagrams| datagrams.link.is_up());
        let switched = up != by_udp;
        if switched {
            by_udp = up;
            if let Some(datagrams) = &mut datagrams {
                datagrams.begin(Instant::now());
                // The client is told when video is back on the connection, and where over UDP it would go on.
                let back = format!(r#"["path","tcp",{}]"#, datagrams.pieces);
                if !by_udp && connection.write(&ws::frame(ws::TEXT, back.as_bytes())).is_err() {
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
                udp.piece(&piece, kbps);
                (left, sending) = (1, sending + ready.elapsed());
            }
            (Next::Sound(packet), Some(udp)) => udp.sound(&packet, kbps),
            (Next::Sound(packet), None) => {
                let message = ws::frame(ws::BINARY, &packet);
                if connection.write(&message).is_err() {
                    break;
                }
                written += message.len() as u64;
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
        kbps = verdict.kbps;
        // Over UDP the spare datagrams take their share of the rate.
        let share = udp.as_ref().map_or(1.0, |udp| udp.share());
        shared.kbps.store((kbps as f64 * share) as u32, Ordering::Relaxed);
        shared.climbing.store(verdict.climbing, Ordering::Relaxed);
        if let Some(udp) = &mut udp
            && udp.needs_fresh(now, reading.round_trip.unwrap_or_default())
        {
            shared.fresh.store(true, Ordering::Relaxed);
        }
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
