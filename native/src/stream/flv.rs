//! FLV, the container the video reaches the page in (index.html reads it): a header, then one tag for each
//! decoder configuration and each frame.

use crate::ws;

/// The stream's header: "FLV", version 1, video only, the header's length (9), and the size of the tag before
/// the first, which there is none of.
pub const HEADER: &[u8] = b"FLV\x01\x01\x00\x00\x00\x09\x00\x00\x00\x00";
/// A tag's type: video. The page tells video by it among the binary messages (see ws.rs).
const VIDEO: u8 = ws::VIDEO;
/// A video tag's first byte: the frame's kind (1 a keyframe, 2 another) and the codec (7, H.264).
const KEYFRAME: u8 = 0x17;
const INTERFRAME: u8 = 0x27;
/// The bytes of a tag before its data, and of the size that follows it.
const TAG_HEAD: u32 = 11;
const VIDEO_HEAD: u32 = 5;

/// What a video tag carries.
#[derive(Clone, Copy)]
pub enum Packet {
    /// How to decode what follows (an avcC record); counts as a keyframe.
    Configuration,
    /// One frame, and whether it is a keyframe.
    Frame { keyframe: bool },
}

/// One video tag, at `time` milliseconds into the stream.
pub fn tag(packet: Packet, time: u32, payload: &[u8]) -> Vec<u8> {
    let (kind, packet_type) = match packet {
        Packet::Configuration => (KEYFRAME, 0),
        Packet::Frame { keyframe } => (if keyframe { KEYFRAME } else { INTERFRAME }, 1),
    };
    let size = payload.len() as u32 + VIDEO_HEAD;
    let time = time.to_be_bytes();
    let mut tag = Vec::with_capacity(payload.len() + (TAG_HEAD + VIDEO_HEAD + 4) as usize);
    tag.push(VIDEO);
    tag.extend(&size.to_be_bytes()[1..]);
    tag.extend([time[1], time[2], time[3], time[0], 0, 0, 0]); // the time's lower 24 bits, its top 8, the stream
    tag.extend([kind, packet_type, 0, 0, 0]); // no composition offset: frames are shown as they come
    tag.extend(payload);
    tag.extend((size + TAG_HEAD).to_be_bytes());
    tag
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_has_its_head_its_data_and_its_size_behind() {
        let tag = tag(Packet::Frame { keyframe: false }, 0x0102_0304, b"abc");
        assert_eq!(tag[..11], [9, 0, 0, 8, 2, 3, 4, 1, 0, 0, 0]);
        assert_eq!(tag[11..19], [0x27, 1, 0, 0, 0, b'a', b'b', b'c']);
        assert_eq!(tag[19..], 19u32.to_be_bytes());
        assert_eq!(super::tag(Packet::Configuration, 0, b"")[11..13], [0x17, 0]);
    }
}
