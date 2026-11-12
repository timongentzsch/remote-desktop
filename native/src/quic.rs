//! The UDP side: a QUIC endpoint that the client page opens a WebTransport session to, so that video need not
//! wait behind a lost packet. One thread owns the socket and every connection; a session reaches its client
//! through a `Link`.
//!
//! Of HTTP/3 it speaks only what a browser needs before it opens the session: the settings that allow one, and
//! "200" to the request for it. It does not read that request. Who a client is, is told by the token its
//! datagrams begin with, which only a logged-in session's page has been given.

use std::collections::{HashMap, VecDeque};
use std::io::{self, ErrorKind};
use std::net::UdpSocket;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use quinn_proto::congestion::{Controller, ControllerFactory};
use quinn_proto::crypto::rustls::QuicServerConfig;
use quinn_proto::{
    Connection, ConnectionHandle, DatagramEvent, Dir, EndpointConfig, Event, ServerConfig, StreamEvent,
    TransportConfig, VarInt,
};
use rcgen::{CertificateParams, KeyPair};
use ring::rand::SecureRandom;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use time::OffsetDateTime;

use crate::certificate;
use crate::rate::Mark;

/// Browsers accept a certificate by its fingerprint alone, without anyone having to trust it first, if it is
/// valid for no more than two weeks. So the endpoint makes its own, and a new one every day.
const VALID_DAYS: i64 = 12;
const RENEWAL: Duration = Duration::from_secs(24 * 60 * 60);

/// The first bytes on the stream that carries this side's HTTP/3 settings: the stream's kind (control), then a
/// SETTINGS frame of pairs. Browsers of different ages ask for different ones before they open a session.
#[rustfmt::skip]
const SETTINGS: &[u8] = &[
    0x00, 0x04, 28,
    0x08, 1,                                              // extended CONNECT
    0x33, 1,                                              // datagrams
    0x80, 0xFF, 0xD2, 0x77, 1,                            // datagrams, as older drafts named it
    0xAB, 0x60, 0x37, 0x42, 1,                            // WebTransport, as older drafts named it
    0xAB, 0x60, 0x37, 0x43, 1,                            // sessions at a time, as older drafts named it
    0xC0, 0x00, 0x00, 0x00, 0xC6, 0x71, 0x70, 0x6A, 1,    // sessions at a time
];
/// The answer to a browser's request for a session: a HEADERS frame saying ":status: 200" and, for browsers
/// that speak the protocol's older draft, that this side does too.
const ACCEPTED: &[u8] = b"\x01\x29\x00\x00\xD9\x27\x15sec-webtransport-http3-draft\x07draft02";
/// A session's datagrams begin with the number of the stream it was asked for on, divided by four. A browser
/// asks on its first stream, so that is 0.
const SESSION: u8 = 0;
/// About what a packet adds to the datagram it carries, in bytes.
const WRAPPING: u64 = 30;

/// What a client's datagrams are made of: the token, the number of the last piece of video it could use, and
/// the number of the first it is missing (`NOTHING` if none).
const TOKEN: usize = 16;
const STATUS: usize = TOKEN + 8;
pub const NOTHING: u32 = u32::MAX;
/// A client says how it is doing many times a second. One that has said nothing for this long cannot be
/// reached this way any more, or its page is hidden and its browser lets it do little.
const SILENCE: Duration = Duration::from_millis(1500);
/// How long a connection may take to show a token before it is closed, and how many connections there may be
/// at a time. Further ones go unanswered.
const STRANGER: Duration = Duration::from_secs(10);
const MOST_CLIENTS: usize = 64;
/// A connection that takes no datagrams of at least this size cannot carry video.
const LEAST_ROOM: usize = 600;
/// How often a quiet connection is asked whether it is still there, which also keeps it open while its page
/// is hidden and says nothing.
const KEEP_ALIVE: Duration = Duration::from_secs(5);

/// Datagrams leave at this many times the session's bitrate, so that a large frame does not arrive as one
/// burst at whatever is slowest on the way, and a small one is still out within a fraction of its interval.
const PACE: u64 = 4;
/// The most that may leave at once, in bytes: the thread wakes no more often than every millisecond.
const BURST: f64 = 16.0 * 1024.0;
/// How much may wait to be sent to one client. More than this is older than is worth sending.
const WAITING: usize = 4 << 20;

/// How many round trips late a packet is given up as lost, and how many later ones may arrive before it. The
/// usual three would take packets that merely changed places on the way for lost.
const LATE: f32 = 3.0;
const OVERTAKEN: u32 = 64;

/// Larger than any amount under way, and large enough that the connection does not pace by it either.
const WINDOW: u64 = 1 << 40;

/// What the connection knows of how its packets fare, kept where the session can read it.
#[derive(Default)]
struct Counts {
    sent: AtomicU64,
    acknowledged: AtomicU64,
    /// When the latest acknowledgement came, in microseconds since the endpoint started.
    acknowledged_at: AtomicU64,
    lost: AtomicU64,
    /// How many bytes are sent and neither acknowledged nor given up: the connection's own count as of the
    /// latest acknowledgement, and what was handed to it since.
    flight: AtomicU64,
    /// The round trip as it is now, and the shortest of late.
    round_trip_micros: AtomicU32,
    shortest_micros: AtomicU32,
    /// How far single round trips lie from the usual one, on average.
    swing_micros: AtomicU32,
}

/// QUIC's own congestion control is left out of it: the session decides how much it sends (see rate.rs). This
/// only counts.
struct Counting {
    counts: Arc<Counts>,
    started: Instant,
    /// The shortest round trip since `turned`, and that of the stretch before, in microseconds. Older ones are
    /// forgotten: the way may have become longer for good, as when a relay takes over from a direct path.
    recent: u32,
    earlier: u32,
    turned: Instant,
}

/// How long a stretch is of which the shortest round trip is remembered. The one before it counts as well.
const MEMORY: Duration = Duration::from_secs(10);

impl Counting {
    fn new(counts: Arc<Counts>, started: Instant, now: Instant) -> Self {
        Self { counts, started, recent: u32::MAX, earlier: u32::MAX, turned: now }
    }
}

impl Controller for Counting {
    fn on_sent(&mut self, _: Instant, bytes: u64, _: u64) {
        self.counts.sent.fetch_add(bytes, Ordering::Relaxed);
    }
    fn on_ack(&mut self, now: Instant, sent: Instant, bytes: u64, _: bool, rtt: &quinn_proto::RttEstimator) {
        self.counts.acknowledged.fetch_add(bytes, Ordering::Relaxed);
        self.counts.acknowledged_at.store(now.duration_since(self.started).as_micros() as u64, Ordering::Relaxed);
        self.counts.round_trip_micros.store(rtt.get().as_micros() as u32, Ordering::Relaxed);
        let took = now.duration_since(sent);
        if now.duration_since(self.turned) >= MEMORY {
            (self.earlier, self.recent, self.turned) = (self.recent, u32::MAX, now);
        }
        self.recent = self.recent.min(took.as_micros() as u32);
        self.counts.shortest_micros.store(self.recent.min(self.earlier), Ordering::Relaxed);
        let off = took.abs_diff(rtt.get()).as_micros() as u32;
        let swing = self.counts.swing_micros.load(Ordering::Relaxed);
        self.counts.swing_micros.store(swing - swing / 8 + off / 8, Ordering::Relaxed);
    }
    fn on_end_acks(&mut self, _: Instant, in_flight: u64, _: bool, _: Option<u64>) {
        self.counts.flight.store(in_flight, Ordering::Relaxed);
    }
    fn on_congestion_event(&mut self, _: Instant, _: Instant, _: bool, lost_bytes: u64) {
        self.counts.lost.fetch_add(lost_bytes, Ordering::Relaxed);
        let less = |flight: u64| Some(flight.saturating_sub(lost_bytes));
        let _ = self.counts.flight.try_update(Ordering::Relaxed, Ordering::Relaxed, less);
    }
    fn on_mtu_update(&mut self, _: u16) {}
    fn window(&self) -> u64 {
        WINDOW
    }
    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(Self { counts: self.counts.clone(), ..*self })
    }
    fn initial_window(&self) -> u64 {
        WINDOW
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}

/// Makes each new connection's counter and keeps hold of its counts until the endpoint's thread, which is
/// what accepts the connection, takes them.
struct Counters {
    started: Instant,
    latest: Mutex<Option<Arc<Counts>>>,
}

impl ControllerFactory for Counters {
    fn build(self: Arc<Self>, now: Instant, _: u16) -> Box<dyn Controller> {
        let counts = Arc::new(Counts::default());
        *self.latest.lock().unwrap() = Some(counts.clone());
        Box::new(Counting::new(counts, self.started, now))
    }
}

/// A certificate made for this endpoint alone, and its SHA-256 fingerprint.
fn certificate() -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>, [u8; 32]), String> {
    let mut parameters = CertificateParams::new(certificate::names()).map_err(|error| error.to_string())?;
    let now = OffsetDateTime::now_utc();
    parameters.not_before = now - time::Duration::days(1);
    parameters.not_after = now + time::Duration::days(VALID_DAYS);
    let key = KeyPair::generate().map_err(|error| error.to_string())?;
    let certificate = parameters.self_signed(&key).map_err(|error| error.to_string())?;
    let fingerprint = ring::digest::digest(&ring::digest::SHA256, certificate.der());
    let key = PrivateKeyDer::try_from(key.serialize_der()).map_err(|error| error.to_string())?;
    Ok((certificate.der().clone(), key, fingerprint.as_ref().try_into().unwrap()))
}

/// The endpoint's setup with a new certificate, and that certificate's fingerprint.
fn configuration(counters: &Arc<Counters>) -> Result<(Arc<ServerConfig>, [u8; 32]), String> {
    let (certificate, key, fingerprint) = certificate()?;
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|error| error.to_string())?
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .map_err(|error| error.to_string())?;
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let mut transport = TransportConfig::default();
    transport.congestion_controller_factory(counters.clone());
    transport.datagram_receive_buffer_size(Some(1 << 16));
    transport.datagram_send_buffer_size(WAITING);
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    // A browser opens a handful of streams and says little on them, and nothing here reads them. A stranger is
    // given no more room than that to fill.
    transport.max_concurrent_bidi_streams(VarInt::from_u32(4));
    transport.max_concurrent_uni_streams(VarInt::from_u32(8));
    transport.stream_receive_window(VarInt::from_u32(1 << 16));
    transport.receive_window(VarInt::from_u32(1 << 18));
    // Packets stay at the size every path takes. Trying larger ones pays little, and the connection takes
    // lost large packets for a path that drops them, which a link that merely loses packets is not.
    transport.mtu_discovery_config(None);
    // A packet counts as lost when it is this many round trips late. Nothing is sent again here, so there is
    // no hurry to know; and on a way whose round trip swings, the usual patience of an eighth more takes
    // packets for lost that are merely late, which then looks like a link that loses heavily.
    transport.time_threshold(LATE);
    transport.packet_threshold(OVERTAKEN);
    let mut configuration =
        ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls).map_err(|error| error.to_string())?));
    configuration.transport_config(Arc::new(transport));
    // A client whose address changes opens a new connection (its page tries again by itself). Following it
    // would make the connection count anew, apart from the link.
    configuration.migration(false);
    Ok((Arc::new(configuration), fingerprint))
}

/// What the endpoint's thread and the sessions share.
struct Shared {
    fingerprint: Mutex<[u8; 32]>,
    /// The links of the sessions there are.
    links: Mutex<Vec<Arc<Link>>>,
    /// An eventfd: written to when a link has something for the thread.
    wake: OwnedFd,
    started: Instant,
}

impl Shared {
    fn wake(&self) {
        let one = 1u64.to_ne_bytes();
        unsafe { libc::write(self.wake.as_raw_fd(), one.as_ptr().cast(), one.len()) };
    }
}

/// The way from one session to its client, once the client has opened it.
pub struct Link {
    pub token: [u8; TOKEN],
    shared: Arc<Shared>,
    waiting: Mutex<VecDeque<Vec<u8>>>,
    /// How many bytes are waiting there.
    queued: AtomicU64,
    /// The session's bitrate in kbit/s, by which its datagrams are paced.
    kbps: AtomicU32,
    /// What the client's connection counts, while one is bound to this link.
    counts: Mutex<Option<Arc<Counts>>>,
    /// The largest datagram the connection takes, in bytes.
    room: AtomicU32,
    /// What the client last said: the last piece it could use, the first it is missing, and when it said so,
    /// in microseconds since the endpoint started.
    used: AtomicU32,
    missing: AtomicU32,
    heard: AtomicU64,
    /// Set by the session when it is over.
    over: AtomicBool,
}

/// What a link's connection knows at one moment, for the bitrate to be set by (see `rate::Reading`).
pub struct Fared {
    /// How many bytes are on their way: waiting to leave, or sent and neither acknowledged nor given up.
    pub under_way: u64,
    /// How many bytes were sent, and how many of them count as lost.
    pub sent: u64,
    pub lost: u64,
    pub arrived: Mark,
    pub round_trip: Duration,
    /// The shortest round trip of late: the way's own length, with nothing queued along it.
    pub shortest: Duration,
    /// How far single round trips lie from the usual one, on average: what the way does by itself, as a relay
    /// or a radio link along it does, with nothing of ours queued.
    pub swing: Duration,
}

impl Link {
    /// Whether the client can be reached this way right now.
    pub fn is_up(&self) -> bool {
        let heard = Duration::from_micros(self.heard.load(Ordering::Relaxed));
        self.counts.lock().unwrap().is_some()
            && self.room() >= LEAST_ROOM
            && !heard.is_zero()
            && self.shared.started.elapsed() < heard + SILENCE
    }

    /// The largest datagram that can be sent, in bytes.
    pub fn room(&self) -> usize {
        self.room.load(Ordering::Relaxed) as usize
    }

    /// Queues datagrams for the client. They leave paced by the bitrate given.
    pub fn send(&self, datagrams: impl IntoIterator<Item = Vec<u8>>, kbps: u32) {
        self.kbps.store(kbps, Ordering::Relaxed);
        let mut waiting = self.waiting.lock().unwrap();
        let mut size = self.queued.load(Ordering::Relaxed) as usize;
        for datagram in datagrams {
            size += datagram.len();
            waiting.push_back(datagram);
        }
        // What the pace does not get out in time is not worth sending any more: the oldest goes.
        while size > WAITING {
            size -= waiting.pop_front().map_or(0, |datagram| datagram.len());
        }
        self.queued.store(size as u64, Ordering::Relaxed);
        drop(waiting);
        self.shared.wake();
    }

    /// How what was sent has fared. `None` while no client is bound.
    pub fn fared(&self, now: Instant) -> Option<Fared> {
        let counts = self.counts.lock().unwrap().clone()?;
        let (sent, acknowledged) = (counts.sent.load(Ordering::Relaxed), counts.acknowledged.load(Ordering::Relaxed));
        // With nothing under way the count is as fresh as can be, however long ago the last word came.
        let lost = counts.lost.load(Ordering::Relaxed);
        let under_way = self.queued.load(Ordering::Relaxed) + counts.flight.load(Ordering::Relaxed);
        let at = self.shared.started + Duration::from_micros(counts.acknowledged_at.load(Ordering::Relaxed));
        Some(Fared {
            under_way,
            sent,
            lost,
            arrived: (if under_way == 0 { now } else { at }, acknowledged),
            round_trip: Duration::from_micros(counts.round_trip_micros.load(Ordering::Relaxed).into()),
            shortest: Duration::from_micros(counts.shortest_micros.load(Ordering::Relaxed).into()),
            swing: Duration::from_micros(counts.swing_micros.load(Ordering::Relaxed).into()),
        })
    }

    /// The last piece of video the client could use, and the first it is missing (`NOTHING` if none).
    pub fn status(&self) -> (u32, u32) {
        (self.used.load(Ordering::Relaxed), self.missing.load(Ordering::Relaxed))
    }

    /// The session is over: the client's connection is closed and the link forgotten.
    pub fn close(&self) {
        self.over.store(true, Ordering::Relaxed);
        self.shared.wake();
    }
}

/// The endpoint, as the rest of the host sees it.
pub struct Endpoint {
    pub port: u16,
    shared: Arc<Shared>,
}

impl Endpoint {
    /// Takes the UDP port of `address` and serves it on a thread of its own.
    pub fn open(address: &str) -> io::Result<Self> {
        let socket = UdpSocket::bind(address)?;
        socket.set_nonblocking(true)?;
        let port = socket.local_addr()?.port();
        let wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if wake < 0 {
            return Err(io::Error::last_os_error());
        }
        let started = Instant::now();
        let counters = Arc::new(Counters { started, latest: None.into() });
        let (configuration, fingerprint) = configuration(&counters).map_err(io::Error::other)?;
        let shared = Arc::new(Shared {
            fingerprint: fingerprint.into(),
            links: Vec::new().into(),
            wake: unsafe { OwnedFd::from_raw_fd(wake) },
            started,
        });
        thread::spawn({
            let shared = shared.clone();
            move || {
                if let Err(error) = serve(socket, &shared, &counters, configuration) {
                    eprintln!("remote-desktop: the UDP side ended: {error}");
                }
            }
        });
        Ok(Self { port, shared })
    }

    /// The fingerprint of the certificate a client has to expect.
    pub fn fingerprint(&self) -> [u8; 32] {
        *self.shared.fingerprint.lock().unwrap()
    }

    /// A link for a new session, which a client opens by showing its token.
    pub fn link(&self) -> Arc<Link> {
        let mut token = [0; TOKEN];
        ring::rand::SystemRandom::new().fill(&mut token).expect("the system gives random bytes");
        let link = Arc::new(Link {
            token,
            shared: self.shared.clone(),
            waiting: VecDeque::new().into(),
            queued: 0.into(),
            kbps: 0.into(),
            counts: None.into(),
            room: 1200.into(),
            used: NOTHING.into(),
            missing: NOTHING.into(),
            heard: 0.into(),
            over: false.into(),
        });
        self.shared.links.lock().unwrap().push(link.clone());
        link
    }
}

/// One browser's connection.
struct Client {
    quic: Connection,
    counts: Arc<Counts>,
    since: Instant,
    /// Whether it has asked for the session and was told yes.
    accepted: bool,
    link: Option<Arc<Link>>,
    /// How many bytes may leave now, and when that was last worked out.
    allowance: f64,
    paced: Instant,
}

impl Client {
    /// Takes what the client sent: its token and how it is doing.
    fn hear(&mut self, datagram: &[u8], shared: &Shared) {
        let [SESSION, status @ ..] = datagram else { return };
        if status.len() != STATUS {
            return;
        }
        if self.link.is_none() {
            let links = shared.links.lock().unwrap();
            let Some(link) = links.iter().find(|link| link.token == status[..TOKEN]) else { return };
            // The newest connection to show a link's token has it: a page may have opened a second one.
            *link.counts.lock().unwrap() = Some(self.counts.clone());
            self.link = Some(link.clone());
        }
        let Some(link) = &self.link else { return };
        let number = |at: usize| u32::from_be_bytes(status[at..at + 4].try_into().unwrap());
        link.used.store(number(TOKEN), Ordering::Relaxed);
        link.missing.store(number(TOKEN + 4), Ordering::Relaxed);
        link.heard.store(shared.started.elapsed().as_micros() as u64, Ordering::Relaxed);
    }

    /// Whether this connection is the one its link counts on.
    fn has_its_link(&self) -> bool {
        self.link.as_ref().is_some_and(|link| {
            link.counts.lock().unwrap().as_ref().is_some_and(|counts| Arc::ptr_eq(counts, &self.counts))
        })
    }

    /// Hands the connection as many of the link's waiting datagrams as the pace allows. Says when the next is
    /// due, if any are left.
    fn pass_on(&mut self, now: Instant) -> Option<Instant> {
        if self.quic.is_closed() || !self.has_its_link() {
            return None; // what waits is for the connection that has the link
        }
        let link = self.link.as_ref()?;
        let pace = (link.kbps.load(Ordering::Relaxed) as u64 * PACE * 125) as f64; // bytes a second
        self.allowance = (self.allowance + pace * now.duration_since(self.paced).as_secs_f64()).min(BURST);
        self.paced = now;
        if let Some(room) = self.quic.datagrams().max_size() {
            link.room.store((room as u32).saturating_sub(1), Ordering::Relaxed); // less the byte that names the session
        }
        let mut waiting = link.waiting.lock().unwrap();
        while let Some(next) = waiting.front() {
            if self.allowance < 0.0 {
                // The debt is paid off at the pace, a millisecond at the least.
                let wait = Duration::from_secs_f64(-self.allowance / pace.max(1.0));
                return Some(now + wait.max(Duration::from_millis(1)));
            }
            self.allowance -= next.len() as f64;
            link.queued.fetch_sub(next.len() as u64, Ordering::Relaxed);
            let datagram: Bytes = [&[SESSION][..], next].concat().into();
            // One too large for the path is simply lost.
            if self.quic.datagrams().send(datagram, true).is_ok() {
                self.counts.flight.fetch_add(next.len() as u64 + WRAPPING, Ordering::Relaxed);
            }
            waiting.pop_front();
        }
        None
    }
}

fn serve(
    socket: UdpSocket,
    shared: &Arc<Shared>,
    counters: &Arc<Counters>,
    configuration: Arc<ServerConfig>,
) -> io::Result<()> {
    let mut endpoint = quinn_proto::Endpoint::new(Arc::new(EndpointConfig::default()), Some(configuration), true, None);
    let mut clients: HashMap<ConnectionHandle, Client> = HashMap::new();
    let (mut packet, mut out) = (vec![0; 1 << 16], Vec::new());
    let mut renewed = Instant::now();
    // The earliest moment something is due without a packet arriving.
    let mut due: Option<Instant> = None;
    loop {
        let patience = due.map_or(RENEWAL, |due| due.saturating_duration_since(Instant::now())).min(RENEWAL);
        let mut polls = [socket.as_raw_fd(), shared.wake.as_raw_fd()].map(|fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
        // Rounded up to the next millisecond, or the thread would spin through the last fraction of one.
        unsafe { libc::poll(polls.as_mut_ptr(), 2, patience.as_millis() as i32 + i32::from(!patience.is_zero())) };
        let mut count = [0u8; 8];
        unsafe { libc::read(shared.wake.as_raw_fd(), count.as_mut_ptr().cast(), count.len()) };
        let now = Instant::now();
        loop {
            let (size, remote) = match socket.recv_from(&mut packet) {
                Ok(received) => received,
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                // A packet that could not be delivered earlier is reported on a later call.
                Err(error) if matches!(error.kind(), ErrorKind::Interrupted | ErrorKind::ConnectionRefused) => continue,
                Err(error) => return Err(error),
            };
            out.clear();
            match endpoint.handle(now, remote, None, None, BytesMut::from(&packet[..size]), &mut out) {
                Some(DatagramEvent::NewConnection(incoming)) if clients.len() >= MOST_CLIENTS => {
                    endpoint.ignore(incoming)
                }
                Some(DatagramEvent::NewConnection(incoming)) => match endpoint.accept(incoming, now, &mut out, None) {
                    Ok((handle, quic)) => {
                        // Accepting made the connection's counter, on this thread.
                        let counts = counters.latest.lock().unwrap().take().unwrap_or_default();
                        let client = Client {
                            quic,
                            counts,
                            since: now,
                            accepted: false,
                            link: None,
                            allowance: 0.0,
                            paced: now,
                        };
                        clients.insert(handle, client);
                    }
                    Err(refused) => {
                        if let Some(reply) = refused.response {
                            let _ = socket.send_to(&out[..reply.size], reply.destination);
                        }
                    }
                },
                Some(DatagramEvent::ConnectionEvent(handle, event)) => {
                    if let Some(client) = clients.get_mut(&handle) {
                        client.quic.handle_event(event);
                    }
                }
                Some(DatagramEvent::Response(reply)) => {
                    let _ = socket.send_to(&out[..reply.size], reply.destination);
                }
                None => {}
            }
        }
        if renewed.elapsed() >= RENEWAL {
            match self::configuration(counters) {
                Ok((fresh, fingerprint)) => {
                    endpoint.set_server_config(Some(fresh));
                    *shared.fingerprint.lock().unwrap() = fingerprint;
                }
                Err(error) => eprintln!("remote-desktop: could not renew the UDP side's certificate: {error}"),
            }
            renewed = Instant::now();
        }
        shared.links.lock().unwrap().retain(|link| !link.over.load(Ordering::Relaxed));
        due = None;
        clients.retain(|&handle, client| {
            if client.quic.poll_timeout().is_some_and(|due| due <= now) {
                client.quic.handle_timeout(now);
            }
            while let Some(event) = client.quic.poll() {
                match event {
                    Event::Connected => {
                        if let Some(control) = client.quic.streams().open(Dir::Uni) {
                            let _ = client.quic.send_stream(control).write(SETTINGS);
                        }
                    }
                    Event::Stream(StreamEvent::Opened { dir: Dir::Bi }) => {
                        while let Some(stream) = client.quic.streams().accept(Dir::Bi) {
                            client.accepted |=
                                !client.accepted && client.quic.send_stream(stream).write(ACCEPTED).is_ok();
                        }
                    }
                    Event::DatagramReceived => {
                        while let Some(datagram) = client.quic.datagrams().recv() {
                            client.hear(&datagram, shared);
                        }
                    }
                    _ => {}
                }
            }
            // A link that another connection took over, or whose session is over, has no use for this one, and
            // a connection that shows no token has none at all.
            let stale =
                client.link.as_ref().is_some_and(|link| !client.has_its_link() || link.over.load(Ordering::Relaxed));
            let stranger = client.link.is_none() && now.duration_since(client.since) > STRANGER;
            if (stale || stranger) && !client.quic.is_closed() {
                client.quic.close(now, VarInt::from_u32(0), Bytes::new());
            }
            let next = client.pass_on(now);
            let mut there = true;
            while let Some(event) = client.quic.poll_endpoint_events() {
                there &= !event.is_drained();
                if let Some(event) = endpoint.handle_event(handle, event) {
                    client.quic.handle_event(event);
                }
            }
            loop {
                out.clear();
                match client.quic.poll_transmit(now, 1, &mut out) {
                    Some(packet) => {
                        let _ = socket.send_to(&out[..packet.size], packet.destination);
                    }
                    None => break,
                }
            }
            if !there
                && client.has_its_link()
                && let Some(link) = &client.link
            {
                *link.counts.lock().unwrap() = None;
            }
            due = [due, next, client.quic.poll_timeout()].into_iter().flatten().min();
            there
        });
    }
}
