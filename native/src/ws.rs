//! The small part of WebSocket (RFC 6455) this server needs: the handshake key and unfragmented frames.

use std::io::{self, Read};

pub const TEXT: u8 = 1;
pub const BINARY: u8 = 2;
const CLOSE: u8 = 8;

/// The first byte of a binary message says what it carries. Video's stream begins with "F" and goes on in tags
/// that begin with 9, which a datagram that carries part of it begins with as well (see fec.rs).
pub const PADDING: u8 = 0;
pub const POINTER: u8 = 7;
pub const SOUND: u8 = 8;
pub const VIDEO: u8 = 9;

/// What RFC 6455 has every server append to the client's key before hashing it.
const ACCEPT_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// The Sec-WebSocket-Accept value answering a client's Sec-WebSocket-Key.
pub fn accept_key(key: &str) -> String {
    let mut hash = sha1_smol::Sha1::new();
    hash.update(key.as_bytes());
    hash.update(ACCEPT_GUID);
    base64(&hash.digest().bytes())
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let group =
            (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            let padding = i > chunk.len();
            out.push(if padding { '=' } else { ALPHABET[(group >> (18 - 6 * i) & 63) as usize] as char });
        }
    }
    out
}

/// Reads the client's next message. `None` when it closes or sends what this server never expects: more than
/// 64 kB at once, or a frame that is not masked as every client's must be.
pub fn read(stream: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut head = [0; 2];
    stream.read_exact(&mut head)?;
    let mut length = (head[1] & 127) as usize;
    if length == 126 {
        let mut extended = [0; 2];
        stream.read_exact(&mut extended)?;
        length = u16::from_be_bytes(extended) as usize;
    }
    if head[0] & 15 == CLOSE || length == 127 || head[1] & 0x80 == 0 {
        return Ok(None);
    }
    let mut mask = [0; 4];
    stream.read_exact(&mut mask)?;
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload)?;
    for (i, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[i % 4];
    }
    Ok(Some(payload))
}

/// One message for the client, as the bytes to send.
pub fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push(0x80 | opcode);
    match payload.len() {
        length @ 0..126 => frame.push(length as u8),
        length @ 126..65536 => {
            frame.push(126);
            frame.extend((length as u16).to_be_bytes());
        }
        length => {
            frame.push(127);
            frame.extend((length as u64).to_be_bytes());
        }
    }
    frame.extend(payload);
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_key_matches_the_rfc_example() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn reads_a_masked_client_frame() {
        // "Hello" as sent by a client, from RFC 6455 section 5.7.
        let frame = [0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58];
        assert_eq!(read(&mut &frame[..]).unwrap().as_deref(), Some(&b"Hello"[..]));
        let close = [0x88, 0x80, 0, 0, 0, 0];
        assert_eq!(read(&mut &close[..]).unwrap(), None);
        let unmasked = [0x81, 0x05, b'H', b'e', b'l', b'l', b'o'];
        assert_eq!(read(&mut &unmasked[..]).unwrap(), None);
    }

    #[test]
    fn writes_each_length_form() {
        let frame = |length: usize| frame(BINARY, &vec![7; length]);
        assert_eq!(frame(5)[..2], [0x82, 5]);
        assert_eq!(frame(300)[..4], [0x82, 126, 1, 44]);
        assert_eq!(frame(70000)[..10], [0x82, 127, 0, 0, 0, 0, 0, 1, 0x11, 0x70]);
        assert_eq!(frame(70000).len(), 70010);
    }
}
