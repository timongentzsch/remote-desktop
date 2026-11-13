//! H.264 encoding through the C interface in encoder.c: on the GPU where it has an encoder, with x264 otherwise.

use std::ffi::c_int;
use std::process::Command;
use std::ptr;
use std::slice;
use std::sync::OnceLock;

#[repr(C)]
struct Raw {
    _opaque: [u8; 0],
}

unsafe extern "C" {
    fn encoder_open(setup: *const Setup, gpu: c_int) -> *mut Raw;
    fn encoder_plane(encoder: *mut Raw, index: c_int, stride: *mut c_int) -> *mut u8;
    fn encoder_headers(
        encoder: *mut Raw,
        sps: *mut *mut u8,
        sps_size: *mut c_int,
        pps: *mut *mut u8,
        pps_size: *mut c_int,
    ) -> c_int;
    fn encoder_encode(
        encoder: *mut Raw,
        fresh: c_int,
        data: *mut *mut u8,
        keyframe: *mut c_int,
        coarse: *mut c_int,
    ) -> c_int;
    fn encoder_set_rate(encoder: *mut Raw, kbps: c_int, fps: c_int, quality: c_int) -> c_int;
    fn encoder_close(encoder: *mut Raw);
}

/// The quality targets, on H.264's scale of quantisers (lower is better): for a picture that moves, and for one
/// that stands still and is refined (see stream/mod.rs). A still picture is looked at closely and costs nothing
/// once it is sharp, so it is made sharper than moving ones, which would cost that much on every frame.
pub const MOVING_QUALITY: u32 = 20;
pub const STILL_QUALITY: u32 = 15;

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
        }
    }
}

/// One plane of the picture to encode: its bytes and the length of a row in them.
pub struct Plane<'a> {
    pub bytes: &'a mut [u8],
    pub stride: usize,
}

pub struct Encoded<'a> {
    /// The access unit, as length-prefixed NAL units.
    pub unit: &'a [u8],
    pub keyframe: bool,
    /// The bitrate cap, not the quality target, decided how good the frame is. Only x264 says so.
    pub coarse: bool,
}

/// How many levels of effort the software encoder has, as in encoder.h: 0 is the least work for a frame.
pub const EFFORTS: u32 = 3;

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
    /// Whether the GPU does the encoding.
    pub gpu: bool,
}

impl Encoder {
    /// Opens the GPU's encoder where there is one that takes this picture, and x264 otherwise.
    pub fn open(setup: Setup) -> Option<Self> {
        let on_gpu = if gpu_can_encode() { Self::open_on(true, setup) } else { None };
        on_gpu.or_else(|| Self::open_on(false, setup))
    }

    fn open_on(gpu: bool, setup: Setup) -> Option<Self> {
        let raw = unsafe { encoder_open(&setup, gpu as c_int) };
        // Built only on success: dropping an `Encoder` closes the pointer it holds.
        let (width, height, full_chroma) = (setup.width as usize, setup.height as usize, setup.full_chroma != 0);
        (!raw.is_null()).then(|| Self { raw, width, height, full_chroma, gpu })
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

    /// The decoder configuration record (avcC) describing this stream.
    pub fn configuration(&mut self) -> Vec<u8> {
        let (mut sps, mut pps) = (ptr::null_mut(), ptr::null_mut());
        let (mut sps_size, mut pps_size) = (0, 0);
        let status = unsafe { encoder_headers(self.raw, &mut sps, &mut sps_size, &mut pps, &mut pps_size) };
        assert!(status == 0 && !sps.is_null() && !pps.is_null(), "the encoder returned no parameter sets");
        let (sps, pps) =
            unsafe { (slice::from_raw_parts(sps, sps_size as usize), slice::from_raw_parts(pps, pps_size as usize)) };

        let mut record = vec![1, sps[1], sps[2], sps[3], 0xFF, 0xE1];
        record.extend((sps.len() as u16).to_be_bytes());
        record.extend(sps);
        record.push(1);
        record.extend((pps.len() as u16).to_be_bytes());
        record.extend(pps);
        if matches!(sps[1], 100 | 110 | 122 | 244) {
            // High profiles also state the chroma format (1 = 4:2:0, 3 = 4:4:4) and 8-bit depth.
            record.extend([0xFC | if self.full_chroma { 3 } else { 1 }, 0xF8, 0xF8, 0]);
        }
        record
    }

    /// Tells the running encoder the bitrate cap and how many frames a second it will get from now on (a frame
    /// may take that much more of the cap the fewer there are), and the quality to aim for. False if the encoder
    /// refused and nothing changed.
    pub fn set_rate(&mut self, kbps: u32, fps: u32, quality: u32) -> bool {
        unsafe { encoder_set_rate(self.raw, kbps as c_int, fps as c_int, quality as c_int) == 0 }
    }

    /// Encodes the picture in `planes`; as a frame that needs none before it, if `fresh`.
    pub fn encode(&mut self, fresh: bool) -> Option<Encoded<'_>> {
        let (mut data, mut keyframe, mut coarse) = (ptr::null_mut(), 0, 0);
        let size = unsafe { encoder_encode(self.raw, fresh as c_int, &mut data, &mut keyframe, &mut coarse) };
        // The access unit stays valid until the next call into the encoder, which needs `&mut self`.
        (size > 0).then(|| Encoded {
            unit: unsafe { slice::from_raw_parts(data, size as usize) },
            keyframe: keyframe != 0,
            coarse: coarse != 0,
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

    use super::super::{REFINE_FPS, Still};
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
                assert!(encoder.set_rate(500, 60, MOVING_QUALITY), "the bitrate cap can change mid-stream");
                assert!(
                    encoder.encode(false).is_some_and(|encoded| !encoded.keyframe),
                    "and the stream simply continues"
                );
            }
        }
    }

    /// The GPU encodes for one session at a time (nvenc.c says why): a second encoder is refused while the first
    /// is open and granted once it is closed.
    #[test]
    fn the_gpu_encodes_for_one_session_at_a_time() {
        let _alone = GPU.lock();
        let open = || Encoder::open_on(true, Setup::new(640, 480, 60, 2000, false, 0));
        let Some(first) = open() else { return }; // this machine's GPU cannot encode
        assert!(open().is_none(), "a second encoder while the first is open");
        drop(first);
        assert!(open().is_some(), "an encoder once the first is closed");
    }

    /// A bitrate cap forces a detailed picture out coarse at first. Encoding it again at the pace of refinement
    /// must improve it and then come to rest, or a still screen would either stay blurry or be re-sent forever.
    #[test]
    fn x264_says_when_the_cap_costs_quality() {
        for effort in 0..EFFORTS {
            // Whether the last of this many encodings of one detailed picture came out coarse.
            let coarse_after = |frames, kbps| {
                let mut encoder =
                    Encoder::open_on(false, Setup::new(640, 480, 60, kbps, false, effort)).expect("x264 opens");
                draw_detail(&mut encoder);
                (0..frames).map(|_| encoder.encode(false).expect("a frame in gives a frame out").coarse).last().unwrap()
            };
            assert!(coarse_after(3, 200), "under a low cap the picture stays coarse (effort {effort})");
            // A first frame is held to a part of what the cap allows at once, so it may be coarse even here.
            assert!(!coarse_after(3, 50000), "under a high one it soon is not (effort {effort})");
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
            assert!(encoder.set_rate(kbps, REFINE_FPS, STILL_QUALITY));
            let (mut sizes, mut still) = (Vec::new(), Still::default());
            still.finished(first, Still::share(kbps));
            loop {
                sizes.push(encoder.encode(false).expect("a frame in gives a frame out").unit.len());
                if still.finished(*sizes.last().unwrap(), Still::share(kbps)) {
                    break;
                }
                assert!(
                    sizes.len() < 200,
                    "the picture never settled (full_chroma={full_chroma} gpu={gpu}): {sizes:?}"
                );
            }
            println!("full_chroma={full_chroma} gpu={gpu}: first frame {first} bytes, then {sizes:?}");
            assert!(sizes.iter().sum::<usize>() > first, "refining adds more detail than the first frame could carry");
            // No frame may take more than the fifth of a second of the cap that the encoders' buffer allows.
            let most = kbps as usize * 1000 / 8 / 5;
            assert!(sizes.iter().all(|&size| size <= most + most / 10), "refinement frames keep to the bitrate cap");
        }
    }
}
