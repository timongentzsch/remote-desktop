//! H.264 encoding through the C wrapper in encoder.c.

use std::ffi::c_int;
use std::ptr;
use std::slice;

#[repr(C)]
struct Raw {
    _opaque: [u8; 0],
}

unsafe extern "C" {
    fn encoder_open(width: c_int, height: c_int, fps: c_int, kbps: c_int, full_chroma: c_int) -> *mut Raw;
    fn encoder_plane(encoder: *mut Raw, index: c_int, stride: *mut c_int) -> *mut u8;
    fn encoder_headers(encoder: *mut Raw, sps: *mut *mut u8, sps_size: *mut c_int, pps: *mut *mut u8, pps_size: *mut c_int) -> c_int;
    fn encoder_encode(encoder: *mut Raw, data: *mut *mut u8, keyframe: *mut c_int) -> c_int;
    fn encoder_set_bitrate(encoder: *mut Raw, kbps: c_int) -> c_int;
    fn encoder_close(encoder: *mut Raw);
}

/// One plane of the picture to encode: its bytes and the length of a row in them.
pub struct Plane<'a> {
    pub bytes: &'a mut [u8],
    pub stride: usize,
}

/// One encoded frame.
pub struct Encoded<'a> {
    /// The access unit, as length-prefixed NAL units.
    pub unit: &'a [u8],
    pub keyframe: bool,
    /// The frame is so small that it can only say "nothing changed". Under a bitrate cap such frames also occur
    /// between ones that add detail, so a still picture is finished only after a run of them.
    pub settled: bool,
}

pub struct Encoder {
    raw: *mut Raw,
    pub width: usize,
    pub height: usize,
    pub full_chroma: bool,
}

impl Encoder {
    pub fn open(width: usize, height: usize, fps: u32, kbps: u32, full_chroma: bool) -> Option<Self> {
        let raw = unsafe { encoder_open(width as c_int, height as c_int, fps as c_int, kbps as c_int, full_chroma as c_int) };
        (!raw.is_null()).then_some(Self { raw, width, height, full_chroma })
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
                Plane { bytes: unsafe { slice::from_raw_parts_mut(pointer, stride as usize * rows) }, stride: stride as usize }
            }
        })
    }

    /// The decoder configuration record (avcC) describing this stream.
    pub fn configuration(&mut self) -> Vec<u8> {
        let (mut sps, mut pps) = (ptr::null_mut(), ptr::null_mut());
        let (mut sps_size, mut pps_size) = (0, 0);
        let status = unsafe { encoder_headers(self.raw, &mut sps, &mut sps_size, &mut pps, &mut pps_size) };
        assert!(status == 0 && !sps.is_null() && !pps.is_null(), "x264 returned no parameter sets");
        let (sps, pps) = unsafe { (slice::from_raw_parts(sps, sps_size as usize), slice::from_raw_parts(pps, pps_size as usize)) };

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

    /// Changes the bitrate cap while running. False if x264 refused, in which case the old cap stays.
    pub fn set_bitrate(&mut self, kbps: u32) -> bool {
        unsafe { encoder_set_bitrate(self.raw, kbps as c_int) == 0 }
    }

    /// Encodes the picture in `planes`.
    pub fn encode(&mut self) -> Option<Encoded<'_>> {
        let (mut data, mut keyframe) = (ptr::null_mut(), 0);
        let size = unsafe { encoder_encode(self.raw, &mut data, &mut keyframe) };
        // The access unit stays valid until the next call into the encoder, which needs `&mut self`.
        // "Nothing changed" costs a few bytes per row of blocks; anything bigger carried picture detail.
        let settled = (size as usize) < 64 + self.width * self.height / 4096;
        (size > 0).then(|| Encoded { unit: unsafe { slice::from_raw_parts(data, size as usize) }, keyframe: keyframe != 0, settled })
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { encoder_close(self.raw) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every frame must come out of the encoder at once: capture only delivers a frame when the screen changes,
    /// so one held back would not be shown until the next change.
    #[test]
    fn each_frame_is_encoded_immediately() {
        for full_chroma in [false, true] {
            let mut encoder = Encoder::open(64, 64, 60, 1000, full_chroma).expect("x264 opens");
            let configuration = encoder.configuration();
            assert_eq!(configuration[0], 1, "an avcC record");
            assert_eq!(configuration[1], if full_chroma { 244 } else { 66 }, "the stream's profile");
            for (frame, expect_keyframe) in [(0, true), (1, false)] {
                for plane in encoder.planes() {
                    plane.bytes.fill(100 + frame * 50);
                }
                let encoded = encoder.encode().expect("a frame in gives a frame out");
                assert!(!encoded.unit.is_empty());
                assert_eq!(encoded.keyframe, expect_keyframe);
            }
            assert!(encoder.set_bitrate(500), "the bitrate cap can change mid-stream");
            assert!(encoder.encode().is_some_and(|encoded| !encoded.keyframe), "and the stream simply continues");
        }
    }

    /// A bitrate cap forces a detailed picture out coarse at first. Encoding it again and again must improve it
    /// and then come to rest, or a still screen would either stay blurry or be re-sent forever.
    #[test]
    fn a_still_picture_is_refined_and_then_settles() {
        let (width, height) = (640, 480);
        for full_chroma in [false, true] {
            let mut encoder = Encoder::open(width, height, 60, 300, full_chroma).expect("x264 opens");
            for (index, plane) in encoder.planes().into_iter().enumerate() {
                for (i, byte) in plane.bytes.iter_mut().enumerate() {
                    let (x, y) = (i % plane.stride, i / plane.stride);
                    *byte = ((x * 7) ^ (y * 13) ^ (index * 90)) as u8;
                }
            }
            let (mut frames, mut bytes, mut settled_in_a_row) = (0, 0, 0);
            while settled_in_a_row < crate::capture::SETTLED_FRAMES {
                let encoded = encoder.encode().expect("a frame in gives a frame out");
                settled_in_a_row = if encoded.settled { settled_in_a_row + 1 } else { 0 };
                frames += 1;
                bytes += encoded.unit.len();
                assert!(frames < 3000, "the picture never settled (full_chroma={full_chroma})");
            }
            println!("full_chroma={full_chroma}: settled after {frames} frames and {} kB", bytes / 1024);
            assert!(frames > 16, "a detailed picture at 300 kbit/s cannot be finished at once");
        }
    }
}
