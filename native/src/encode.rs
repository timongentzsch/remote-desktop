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
    fn encoder_close(encoder: *mut Raw);
}

/// One plane of the picture to encode: its bytes and the length of a row in them.
pub struct Plane<'a> {
    pub bytes: &'a mut [u8],
    pub stride: usize,
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

    /// Encodes the picture in `planes`. Returns the access unit and whether it is a keyframe.
    pub fn encode(&mut self) -> Option<(&[u8], bool)> {
        let (mut data, mut keyframe) = (ptr::null_mut(), 0);
        let size = unsafe { encoder_encode(self.raw, &mut data, &mut keyframe) };
        // The access unit stays valid until the next call into the encoder, which needs `&mut self`.
        (size > 0).then(|| (unsafe { slice::from_raw_parts(data, size as usize) }, keyframe != 0))
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { encoder_close(self.raw) }
    }
}
