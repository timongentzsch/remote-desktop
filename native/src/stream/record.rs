//! Decoder configuration records, which tell a decoder how to decode the stream before its first frame: avcC
//! for H.264 and hvcC for HEVC (ISO/IEC 14496-15). The page hands them to its decoder as they are, and reads
//! the codec's string for it from them (index.html).

/// HEVC's NAL unit types of the parameter sets, as nal.h has them.
const HEVC_VPS: u8 = 32;
const HEVC_SPS: u8 = 33;
const HEVC_PPS: u8 = 34;
/// Each NAL unit in the stream has its length in four bytes in front (see nal.c).
const LENGTH_SIZE_MINUS_ONE: u8 = 3;

/// The avcC record of a stream with these parameter sets (raw, without start code or length).
pub fn avc(sps: &[u8], pps: &[u8], full_chroma: bool) -> Vec<u8> {
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
    }
}
