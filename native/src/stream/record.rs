//! Decoder configuration records, which tell a decoder how to decode the stream before its first frame: avcC
//! for H.264 and hvcC for HEVC (ISO/IEC 14496-15). The page hands them to its decoder as they are, and reads
//! the codec's string for it from them (index.html).

/// HEVC's NAL unit types of the parameter sets, as nal.h has them.
const HEVC_VPS: u8 = 32;
const HEVC_SPS: u8 = 33;
const HEVC_PPS: u8 = 34;
/// Each NAL unit in the stream has its length in four bytes in front (see nal.c).
const LENGTH_SIZE_MINUS_ONE: u8 = 3;

/// The avcC record of a stream with these parameter sets (raw, without start code or length). Its SPS says that
/// frames may be left out (see `allowing_gaps`).
pub fn avc(sps: &[u8], pps: &[u8], full_chroma: bool) -> Vec<u8> {
    let sps = &allowing_gaps(sps);
    let mut record = vec![1, sps[1], sps[2], sps[3], 0xFC | LENGTH_SIZE_MINUS_ONE, 0xE1];
    record.extend((sps.len() as u16).to_be_bytes());
    record.extend(sps);
    record.push(1);
    record.extend((pps.len() as u16).to_be_bytes());
    record.extend(pps);
    if matches!(sps[1], 100 | 110 | 122 | 244) {
        // High profiles also state the chroma format (1 = 4:2:0, 3 = 4:4:4) and 8-bit depth.
        record.extend([0xFC | if full_chroma { 3 } else { 1 }, 0xF8, 0xF8, 0]);
    }
    record
}

/// The hvcC record of an HEVC stream with these parameter sets (raw, without start code or length): 4:2:0,
/// 8-bit, one temporal layer, frames at no fixed rate.
pub fn hevc(vps: &[u8], sps: &[u8], pps: &[u8]) -> Vec<u8> {
    // The SPS's general profile, tier and level: 12 bytes after its two-byte header and one byte of ids,
    // read where emulation prevention has not put bytes between.
    let rbsp = unescape(&sps[2..]);
    let mut record = vec![1];
    record.extend(&rbsp[1..13]);
    record.extend([0xF0, 0x00]); // min_spatial_segmentation_idc: none
    record.push(0xFC); // parallelismType: none
    record.push(0xFC | 1); // chroma format: 4:2:0
    record.extend([0xF8, 0xF8]); // 8-bit luma and chroma
    record.extend([0, 0]); // avgFrameRate: not said
    // constantFrameRate 0, numTemporalLayers 1, temporalIdNested 1, and the length's size.
    record.push(1 << 3 | 1 << 2 | LENGTH_SIZE_MINUS_ONE);
    record.push(3);
    for (unit, kind) in [(vps, HEVC_VPS), (sps, HEVC_SPS), (pps, HEVC_PPS)] {
        record.push(0x80 | kind); // the array has every unit of the kind
        record.extend(1u16.to_be_bytes());
        record.extend((unit.len() as u16).to_be_bytes());
        record.extend(unit);
    }
    record
}

/// An H.264 SPS that says gaps in the frame numbers are meant: a client that lost frames goes on from one sent
/// before them (see session/datagrams.rs), and a decoder told otherwise takes the gap for damage. Chromium's
/// own decoder then let a fifth of the frames after such gaps go unseen. The flag follows the profile's fields,
/// the frame number's and picture order's sizes and the number of reference frames; an SPS laid out in a way
/// not read here (a scaling matrix, picture order type 1) is left as it is.
fn allowing_gaps(sps: &[u8]) -> Vec<u8> {
    let mut rbsp = unescape(&sps[1..]);
    let flag = (|| {
        let mut bits = Bits { bytes: &rbsp, at: 0 };
        let profile = bits.read(8)?;
        bits.read(16)?; // constraints and level
        bits.exp_golomb()?; // seq_parameter_set_id
        if matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135) {
            if bits.exp_golomb()? == 3 {
                bits.read(1)?; // separate_colour_plane_flag
            }
            bits.exp_golomb()?; // bit depths
            bits.exp_golomb()?;
            bits.read(1)?; // qpprime_y_zero_transform_bypass_flag
            if bits.read(1)? == 1 {
                return None; // a scaling matrix
            }
        }
        bits.exp_golomb()?; // log2_max_frame_num_minus4
        match bits.exp_golomb()? {
            0 => {
                bits.exp_golomb()?; // log2_max_pic_order_cnt_lsb_minus4
            }
            2 => {}
            _ => return None,
        }
        bits.exp_golomb()?; // max_num_ref_frames
        Some(bits.at)
    })();
    match flag {
        Some(at) if at / 8 < rbsp.len() => {
            rbsp[at / 8] |= 0x80 >> (at % 8);
            [&sps[..1], &escape(&rbsp)].concat()
        }
        _ => sps.to_vec(),
    }
}

/// Reads an RBSP bit by bit, most significant first.
struct Bits<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn read(&mut self, count: usize) -> Option<u32> {
        let mut value = 0;
        for _ in 0..count {
            let byte = self.bytes.get(self.at / 8)?;
            value = value << 1 | (byte >> (7 - self.at % 8) & 1) as u32;
            self.at += 1;
        }
        Some(value)
    }

    /// An unsigned Exp-Golomb number.
    fn exp_golomb(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.read(1)? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some((1 << zeros) - 1 + self.read(zeros)?)
    }
}

/// An RBSP with emulation prevention: a 3 before any byte up to 3 that follows two zero bytes.
fn escape(rbsp: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(rbsp.len() + rbsp.len() / 2);
    let mut zeros = 0;
    for &byte in rbsp {
        if zeros >= 2 && byte <= 3 {
            bytes.push(3);
            zeros = 0;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        bytes.push(byte);
    }
    bytes
}

/// A NAL unit's payload without the emulation prevention bytes: a 3 after two zero bytes.
fn unescape(bytes: &[u8]) -> Vec<u8> {
    let mut plain = Vec::with_capacity(bytes.len());
    let mut zeros = 0;
    for &byte in bytes {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        plain.push(byte);
    }
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_hevc_record_carries_the_profile_and_the_three_sets() {
        // An SPS's start as vaapi_hevc.c writes it: header, ids, then Main profile, compatible with Main and Main
        // 10, progressive frames only (B0), with emulation prevention after the first two zero constraint bytes,
        // and level 150.
        let sps =
            [0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x00, 0xB0, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x96, 0xA0];
        let record = hevc(&[0x40, 0x01, 0x0C], &sps, &[0x44, 0x01, 0xC1]);
        assert_eq!(record[..13], [1, 0x01, 0x60, 0, 0, 0, 0xB0, 0, 0, 0, 0, 0, 150], "profile, tier and level");
        assert_eq!(record[22], 3, "three arrays");
        assert_eq!(record[23..28], [0x80 | 32, 0, 1, 0, 3], "the VPS first");
        assert_eq!(unescape(&[0, 0, 3, 1, 0, 0, 3, 0]), [0, 0, 1, 0, 0, 0]);
        assert_eq!(escape(&[0, 0, 1, 0, 0, 0]), [0, 0, 3, 1, 0, 0, 3, 0]);
    }

    /// x264's SPS at 1280x720 with 16 frames kept, and NVENC's (High profile, picture order type 2): the gaps
    /// flag sits after the number of reference frames, and the bytes around it stay as they were.
    #[test]
    fn an_avc_record_allows_gaps_in_the_frame_numbers() {
        let hex = |text: &str| {
            (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap()).collect::<Vec<_>>()
        };
        let x264 = hex("67640032ac4c2202802dd37010101a50000003001000000788f1832118");
        assert_eq!(allowing_gaps(&x264), hex("67640032ac4c2302802dd37010101a50000003001000000788f1832118"));
        let nvenc = hex("67640032ac2b0880a00b7602dc0404069400000fa00007530210");
        let allowed = allowing_gaps(&nvenc);
        assert_eq!(allowed.len(), nvenc.len());
        assert_eq!(allowed.iter().zip(&nvenc).filter(|(a, b)| a != b).count(), 1, "one bit set");
        assert_eq!(allowing_gaps(&allowed), allowed, "and set once");
        let record = avc(&x264, &[0x68, 0xEE], false);
        assert_eq!(record[8..8 + x264.len()], hex("67640032ac4c2302802dd37010101a50000003001000000788f1832118"));
    }
}
