//! H.264 encoding through the C interface in encoder.c: on the GPU where it has an encoder, with x264 otherwise.

use std::ffi::{CStr, c_char, c_int};
use std::process::Command;
use std::ptr;
use std::slice;
use std::sync::OnceLock;

use super::record;

#[repr(C)]
struct Raw {
    _opaque: [u8; 0],
}

unsafe extern "C" {
    fn encoder_open(setup: *const Setup, gpu: c_int) -> *mut Raw;
    fn encoder_plane(encoder: *mut Raw, index: c_int, stride: *mut c_int) -> *mut u8;
    fn encoder_headers(encoder: *mut Raw, sets: *mut *mut u8, sizes: *mut c_int) -> c_int;
    fn encoder_codec(encoder: *mut Raw) -> c_int;
    fn encoder_encode(
        encoder: *mut Raw,
        fresh: c_int,
        data: *mut *mut u8,
        keyframe: *mut c_int,
        flags: *mut c_int,
    ) -> c_int;
    fn encoder_set_rate(encoder: *mut Raw, kbps: c_int, fps: c_int, quality: c_int, buffer: c_int) -> c_int;
    fn encoder_name(encoder: *mut Raw) -> *const c_char;
    fn encoder_close(encoder: *mut Raw);
}

/// The quality targets, on H.264's scale of quantisers (lower is better): for a picture that moves, and for one
/// that stands still and is refined (see stream/mod.rs). A still picture is looked at closely and costs nothing
/// once it is sharp, so it is made sharper than moving ones, which would cost that much on every frame.
pub const MOVING_QUALITY: u32 = 20;
pub const STILL_QUALITY: u32 = 15;

/// For how many milliseconds of the bitrate cap one frame may take what it allows, at most. A frame that takes
/// more of the link's time than the client has patience for holds up those behind it, so the stream allows a
/// moving picture's frames less than this (see stream/mod.rs).
pub const LONGEST_BUFFER: u32 = 200;

/// What an encoder is opened with, as `struct encoder_setup` in encoder.h.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Setup {
    pub width: c_int,
    pub height: c_int,
    pub fps: c_int,
    pub kbps: c_int,
    pub quality: c_int,
    pub full_chroma: c_int,
    /// For x264: 0 is the least work for a frame, `EFFORTS - 1` the most.
    pub effort: c_int,
    /// The client decodes HEVC, which the GPU's encoder then uses where it has it.
    pub hevc: c_int,
    /// For how many milliseconds of the bitrate cap one frame may take what it allows.
    pub buffer: c_int,
}

impl Setup {
    pub fn new(width: usize, height: usize, fps: u32, kbps: u32, full_chroma: bool, effort: u32) -> Self {
        let int = |value: usize| value as c_int;
        Self {
            width: int(width),
            height: int(height),
            fps: fps as c_int,
            kbps: kbps as c_int,
            quality: MOVING_QUALITY as c_int,
            full_chroma: full_chroma as c_int,
            effort: effort as c_int,
            hevc: 0,
            buffer: LONGEST_BUFFER as c_int,
        }
    }
}

/// What an encoder says of a frame, as encoder.h has it.
const FRAME_COARSE: c_int = 1;
const FRAME_HELD_BACK: c_int = 2;
const FRAME_POOR: c_int = 4;

/// What the stream is encoded as, as nal.h numbers it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Codec {
    H264,
    Hevc,
}

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::Hevc => "HEVC",
        }
    }
}

/// The most parameter sets a codec has, as encoder.h says: HEVC's VPS, SPS and PPS.
const MOST_PARAMETER_SETS: usize = 3;

/// One plane of the picture to encode: its bytes and the length of a row in them.
pub struct Plane<'a> {
    pub bytes: &'a mut [u8],
    pub stride: usize,
}

pub struct Encoded<'a> {
    /// The access unit, as length-prefixed NAL units.
    pub unit: &'a [u8],
    pub keyframe: bool,
    /// The bitrate cap, not the quality target, decided how good the frame is (x264 and VA-API say so), and its
    /// budget held the frame back so far that its size says nothing of how good the picture is (VA-API, which
    /// keeps the budget itself, says so).
    pub coarse: bool,
    pub held_back: bool,
    /// The frame's quantiser is so coarse that a smaller picture would show more (all three say so).
    pub poor: bool,
}

/// How many levels of effort the software encoder has, as in encoder.h: 0 is the least work for a frame.
pub const EFFORTS: u32 = 4;

/// The command-line argument that makes this program report, by its exit status, whether the GPU can encode.
pub const PROBE: &str = "probe-gpu";

/// Whether this machine's GPU can encode. A child process finds out, once: trying loads the GPU's libraries,
/// and on a machine whose GPU has no encoder they would stay in the service's memory for nothing.
fn gpu_can_encode() -> bool {
    static ANSWER: OnceLock<bool> = OnceLock::new();
    *ANSWER.get_or_init(|| {
        std::env::current_exe()
            .and_then(|program| Command::new(program).arg(PROBE).status())
            .is_ok_and(|status| status.success())
    })
}

/// The picture the probe opens an encoder for: width, height, frame rate and bitrate.
const PROBE_PICTURE: (usize, usize, u32, u32) = (256, 256, 30, 1000);

/// What the `PROBE` command answers: whether a small ordinary picture can be encoded on the GPU.
pub fn probe() -> bool {
    let (width, height, fps, kbps) = PROBE_PICTURE;
    Encoder::open_on(true, Setup::new(width, height, fps, kbps, false, 0)).is_some()
}

pub struct Encoder {
    raw: *mut Raw,
    pub width: usize,
    pub height: usize,
    pub full_chroma: bool,
    /// Whether the GPU does the encoding, and which encoder it is: "x264", "nvenc" or "vaapi".
    pub gpu: bool,
    pub name: &'static str,
    pub codec: Codec,
}

impl Encoder {
    /// Opens the GPU's encoder where there is one that takes this picture, and x264 otherwise. Where the colour
    /// detail `may_give_way`, the GPU's encoder at 4:2:0 is preferred to x264 at 4:4:4: it saves more time than
    /// full colour detail costs.
    pub fn open(setup: Setup, colour_may_give_way: bool) -> Option<Self> {
        let reduced = Setup { full_chroma: 0, ..setup };
        let on_gpu = gpu_can_encode()
            .then(|| {
                Self::open_on(true, setup).or_else(|| colour_may_give_way.then(|| Self::open_on(true, reduced))?)
            })
            .flatten();
        on_gpu.or_else(|| Self::open_on(false, setup))
    }

    fn open_on(gpu: bool, setup: Setup) -> Option<Self> {
        let raw = unsafe { encoder_open(&setup, gpu as c_int) };
        // Built only on success: dropping an `Encoder` closes the pointer it holds.
        let (width, height, full_chroma) = (setup.width as usize, setup.height as usize, setup.full_chroma != 0);
        if raw.is_null() {
            return None;
        }
        // The name is a string constant of the C side's.
        let name = unsafe { CStr::from_ptr(encoder_name(raw)) }.to_str().unwrap_or("?");
        let codec = if unsafe { encoder_codec(raw) } == 1 { Codec::Hevc } else { Codec::H264 };
        Some(Self { raw, width, height, full_chroma, gpu, name, codec })
    }

    /// The Y, U and V planes to fill before calling `encode`.
    pub fn planes(&mut self) -> [Plane<'_>; 3] {
        let chroma_rows = if self.full_chroma { self.height } else { self.height / 2 };
        [self.height, chroma_rows, chroma_rows].map({
            let mut index = 0;
            move |rows| {
                let mut stride = 0;
                // The wrapper owns the planes for the encoder's lifetime; each is at least stride * rows bytes.
                let pointer = unsafe { encoder_plane(self.raw, index, &mut stride) };
                index += 1;
                Plane {
                    bytes: unsafe { slice::from_raw_parts_mut(pointer, stride as usize * rows) },
                    stride: stride as usize,
                }
            }
        })
    }

    /// The decoder configuration record describing this stream (see record.rs).
    pub fn configuration(&mut self) -> Vec<u8> {
        let (mut sets, mut sizes) = ([ptr::null_mut(); MOST_PARAMETER_SETS], [0; MOST_PARAMETER_SETS]);
        let count = unsafe { encoder_headers(self.raw, sets.as_mut_ptr(), sizes.as_mut_ptr()) };
        let expected: usize = if self.codec == Codec::Hevc { 3 } else { 2 };
        assert!(
            count == expected as c_int && sets[..expected].iter().all(|set| !set.is_null()),
            "the encoder returned no parameter sets"
        );
        // The encoder keeps them while it is open.
        let set = |at: usize| unsafe { slice::from_raw_parts(sets[at], sizes[at] as usize) };
        match self.codec {
            Codec::H264 => record::avc(set(0), set(1), self.full_chroma),
            Codec::Hevc => record::hevc(set(0), set(1), set(2)),
        }
    }

    /// Tells the running encoder the bitrate cap and how many frames a second it will get from now on (a frame
    /// gets that much more of the cap the fewer there are), the quality to aim for, and for how many
    /// milliseconds of the cap one frame may take what it allows. False if the encoder refused and nothing
    /// changed.
    pub fn set_rate(&mut self, kbps: u32, fps: u32, quality: u32, buffer: u32) -> bool {
        let (kbps, fps, quality, buffer) = (kbps as c_int, fps as c_int, quality as c_int, buffer as c_int);
        unsafe { encoder_set_rate(self.raw, kbps, fps, quality, buffer) == 0 }
    }

    /// Encodes the picture in `planes`; as a frame that needs none before it, if `fresh`.
    pub fn encode(&mut self, fresh: bool) -> Option<Encoded<'_>> {
        let (mut data, mut keyframe, mut flags) = (ptr::null_mut(), 0, 0);
        let size = unsafe { encoder_encode(self.raw, fresh as c_int, &mut data, &mut keyframe, &mut flags) };
        // The access unit stays valid until the next call into the encoder, which needs `&mut self`.
        (size > 0).then(|| Encoded {
            unit: unsafe { slice::from_raw_parts(data, size as usize) },
            keyframe: keyframe != 0,
            coarse: flags & FRAME_COARSE != 0,
            held_back: flags & FRAME_HELD_BACK != 0,
            poor: flags & FRAME_POOR != 0,
        })
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { encoder_close(self.raw) }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::super::{REFINE_FPS, STILL_BUFFER, Still};
    use super::*;

    /// Held by a test while it uses the GPU's encoder, which only one can have at a time.
    static GPU: Mutex<()> = Mutex::new(());

    /// The encoders to test: x264 at each effort, and the GPU's if this machine has one.
    fn encoders(width: usize, height: usize, kbps: u32, full_chroma: bool) -> Vec<Encoder> {
        let software = (0..EFFORTS).map(|effort| {
            Encoder::open_on(false, Setup::new(width, height, 60, kbps, full_chroma, effort)).expect("x264 opens")
        });
        let hardware = Encoder::open_on(true, Setup::new(width, height, 60, kbps, full_chroma, 0));
        println!(
            "full_chroma={full_chroma}: {}",
            if hardware.is_some() { "testing x264 and the GPU" } else { "testing x264 only (no GPU encoder here)" }
        );
        software.chain(hardware).collect()
    }

    /// Fills the encoder's planes with a picture full of fine detail.
    fn draw_detail(encoder: &mut Encoder) {
        for (index, plane) in encoder.planes().into_iter().enumerate() {
            for (i, byte) in plane.bytes.iter_mut().enumerate() {
                let (x, y) = (i % plane.stride, i / plane.stride);
                *byte = ((x * 7) ^ (y * 13) ^ (index * 90)) as u8;
            }
        }
    }

    /// Every frame must come out of the encoder at once: capture only delivers a frame when the screen changes,
    /// so one held back would not be shown until the next change.
    #[test]
    fn each_frame_is_encoded_immediately() {
        let _alone = GPU.lock();
        for full_chroma in [false, true] {
            // The GPU's encoder has a minimum picture size well above x264's.
            for mut encoder in encoders(256, 256, 1000, full_chroma) {
                let configuration = encoder.configuration();
                assert_eq!(configuration[0], 1, "an avcC record");
                // x264 uses the baseline profile at its least effort and the high one above.
                let profiles: &[u8] = match (full_chroma, encoder.gpu) {
                    (true, _) => &[244],
                    (false, true) => &[100],
                    (false, false) => &[66, 100],
                };
                assert!(profiles.contains(&configuration[1]), "the stream's profile");
                for (frame, expect_keyframe) in [(0, true), (1, false)] {
                    for plane in encoder.planes() {
                        plane.bytes.fill(100 + frame * 50);
                    }
                    let encoded = encoder.encode(false).expect("a frame in gives a frame out");
                    let length = u32::from_be_bytes(encoded.unit[..4].try_into().unwrap()) as usize;
                    assert!(length > 0 && 4 + length <= encoded.unit.len(), "NAL units carry their length in front");
                    assert_eq!(encoded.keyframe, expect_keyframe);
                }
                assert!(encoder.set_rate(500, 60, MOVING_QUALITY, 50), "the bitrate cap can change mid-stream");
                assert!(
                    encoder.encode(false).is_some_and(|encoded| !encoded.keyframe),
                    "and the stream simply continues"
                );
            }
        }
    }

    /// NVIDIA's encoder encodes for one session at a time (nvenc.c says why): a second one is refused while the
    /// first is open and granted once it is closed.
    #[test]
    fn nvenc_encodes_for_one_session_at_a_time() {
        let _alone = GPU.lock();
        let open = || {
            Encoder::open_on(true, Setup::new(640, 480, 60, 2000, false, 0)).filter(|encoder| encoder.name == "nvenc")
        };
        let Some(first) = open() else { return }; // this machine's GPU has no such encoder
        assert!(open().is_none(), "a second encoder while the first is open");
        drop(first);
        assert!(open().is_some(), "an encoder once the first is closed");
    }

    /// For a client that decodes HEVC, a GPU that encodes it does: an IDR, then frames that refer to the one before,
    /// each a single slice, and a record that lists the three parameter sets.
    #[test]
    fn the_gpu_encodes_hevc_for_a_client_that_decodes_it() {
        let _alone = GPU.lock();
        let setup = Setup { hevc: 1, ..Setup::new(640, 480, 60, 2000, false, 0) };
        let Some(mut encoder) = Encoder::open_on(true, setup).filter(|encoder| encoder.codec == Codec::Hevc) else {
            return; // this machine's GPU does not encode HEVC
        };
        let record = encoder.configuration();
        assert_eq!((record[0], record[22]), (1, 3), "an hvcC record with three arrays");
        draw_detail(&mut encoder);
        // HEVC's NAL unit type is in the first byte after the length: IDR_W_RADL is 19, TRAIL_R 1.
        for (frame, expected_type) in [(0, 19), (1, 1), (2, 1)] {
            let encoded = encoder.encode(false).expect("a frame in gives a frame out");
            assert_eq!((encoded.unit[4] >> 1) & 0x3F, expected_type, "frame {frame}");
            assert_eq!(encoded.keyframe, frame == 0);
        }
    }

    /// A bitrate cap forces a detailed picture out coarse at first. Encoding it again at the pace of refinement
    /// must improve it and then come to rest, or a still screen would either stay blurry or be re-sent forever.
    #[test]
    fn x264_says_when_the_cap_costs_quality() {
        for effort in 0..EFFORTS {
            // Whether the last of this many encodings of one detailed picture came out coarse, and poor.
            let after = |frames, kbps| {
                let mut encoder =
                    Encoder::open_on(false, Setup::new(640, 480, 60, kbps, false, effort)).expect("x264 opens");
                draw_detail(&mut encoder);
                let frame = |_| encoder.encode(false).map(|encoded| (encoded.coarse, encoded.poor));
                (0..frames).map(frame).last().unwrap().expect("a frame in gives a frame out")
            };
            assert_eq!(after(1, 200), (true, true), "under a low cap the picture begins poor (effort {effort})");
            assert!(after(3, 200).0, "and stays coarse (effort {effort})");
            // A first frame is held to a part of what the cap allows at once, so it may be coarse even here.
            assert_eq!(after(3, 50000), (false, false), "under a high one it soon is neither (effort {effort})");
        }
    }

    #[test]
    fn a_still_picture_is_refined_and_then_settles() {
        let _alone = GPU.lock();
        let (width, height, kbps) = (640, 480, 2000);
        for mut encoder in [false, true].into_iter().flat_map(|full_chroma| encoders(width, height, kbps, full_chroma))
        {
            let (full_chroma, gpu) = (encoder.full_chroma, encoder.gpu);
            draw_detail(&mut encoder);
            let first = encoder.encode(false).expect("a frame in gives a frame out").unit.len();
            assert!(encoder.set_rate(kbps, REFINE_FPS, STILL_QUALITY, STILL_BUFFER));
            let (mut sizes, mut still) = (Vec::new(), Still::default());
            still.finished(first, Still::share(kbps), false);
            loop {
                let encoded = encoder.encode(false).expect("a frame in gives a frame out");
                let (size, held_back) = (encoded.unit.len(), encoded.held_back);
                sizes.push(size);
                if still.finished(size, Still::share(kbps), held_back) {
                    break;
                }
                assert!(
                    sizes.len() < 200,
                    "the picture never settled (full_chroma={full_chroma} gpu={gpu}): {sizes:?}"
                );
            }
            println!("full_chroma={full_chroma} gpu={gpu}: first frame {first} bytes, then {sizes:?}");
            assert!(sizes.iter().sum::<usize>() > first, "refining adds more detail than the first frame could carry");
            // No frame may take more than its tenth of a second of the cap, which the encoders' buffer allows.
            let most = kbps as usize * STILL_BUFFER as usize / 8;
            assert!(sizes.iter().all(|&size| size <= most + most / 10), "refinement frames keep to the bitrate cap");
        }
    }
}
