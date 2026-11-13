//! The desktop's sound: recorded from the audio server's monitor of the default output and sent as Opus packets.
//!
//! It speaks the PulseAudio protocol, which PipeWire's and PulseAudio's servers both answer.

use std::ffi::CString;
use std::io::{BufReader, Read};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use pulseaudio::protocol;
use unsafe_libopus::{
    OPUS_APPLICATION_RESTRICTED_LOWDELAY, OPUS_OK, OPUS_SET_BITRATE_REQUEST, OpusEncoder, opus_encode,
    opus_encoder_create, opus_encoder_ctl, opus_encoder_destroy,
};

use crate::capture::Shared;

/// The most an encoded packet of sound may take, in bytes, as Opus recommends.
const LARGEST_PACKET: usize = 4000;
/// The sound server matches each reply to its command by a tag; these commands are sent one at a time.
const AUTH_TAG: u32 = 0;
const NAME_TAG: u32 = 1;
const RECORD_TAG: u32 = 2;
/// How often the sound thread looks whether the session has ended, while the sound server sends nothing.
const LOOK: std::time::Duration = std::time::Duration::from_millis(250);
const RATE: u32 = 48000;
const CHANNELS: usize = 2;
/// 20 ms, the usual Opus frame.
const FRAME_SAMPLES: usize = RATE as usize / 50;
const FRAME_BYTES: usize = FRAME_SAMPLES * CHANNELS * 2;
const BITRATE: i32 = 96_000;

/// Receives each encoded packet of sound.
pub type Packets = Box<dyn FnMut(&[u8]) + Send>;

/// Records until `shared.stop` is set and hands each encoded packet to `packet`. Silence is not sent.
pub fn run(shared: Arc<Shared>, mut packet: Packets) -> Result<(), String> {
    let (mut server, version) = connect()?;
    let stream = protocol::RecordStreamParams {
        source_name: Some(c"@DEFAULT_MONITOR@".to_owned()),
        sample_spec: protocol::SampleSpec {
            format: protocol::SampleFormat::S16Le,
            channels: CHANNELS as u8,
            sample_rate: RATE,
        },
        channel_map: protocol::ChannelMap::stereo(),
        // Deliver a frame at a time rather than the server's larger default chunks.
        buffer_attr: protocol::stream::BufferAttr { fragment_size: FRAME_BYTES as u32, ..Default::default() },
        flags: protocol::stream::StreamFlags { adjust_latency: true, ..Default::default() },
        ..Default::default()
    };
    protocol::write_command_message(
        server.get_mut(),
        RECORD_TAG,
        &protocol::Command::CreateRecordStream(stream),
        version,
    )
    .map_err(refused)?;
    protocol::read_reply_message::<protocol::CreateRecordStreamReply>(&mut server, version).map_err(refused)?;

    let mut encoder = Encoder::new()?;
    let (mut samples, mut chunk, mut encoded) = (Vec::new(), Vec::new(), [0; LARGEST_PACKET]);
    while !shared.stop.load(Ordering::Relaxed) {
        if server.buffer().is_empty() && crate::fd::wait_readable([server.get_ref().as_raw_fd()], Some(LOOK)) == [false]
        {
            continue;
        }
        let descriptor = protocol::read_descriptor(&mut server).map_err(refused)?;
        if descriptor.channel == u32::MAX {
            // A notice from the server about the stream, not sound.
            protocol::Command::read_tag_prefixed(&mut server, version).map_err(refused)?;
            continue;
        }
        chunk.resize(descriptor.length as usize, 0);
        server.read_exact(&mut chunk).map_err(|error| format!("lost the audio server: {error}"))?;
        samples.extend(chunk.as_chunks::<2>().0.iter().map(|&sample| i16::from_le_bytes(sample)));
        while samples.len() >= FRAME_SAMPLES * CHANNELS {
            let frame: Vec<i16> = samples.drain(..FRAME_SAMPLES * CHANNELS).collect();
            if frame.iter().any(|&sample| sample != 0) {
                let size = encoder.encode(&frame, &mut encoded)?;
                packet(&encoded[..size]);
            }
        }
    }
    Ok(())
}

/// Connects to the user's audio server and introduces this client.
fn connect() -> Result<(BufReader<UnixStream>, u16), String> {
    let path = pulseaudio::socket_path_from_env().ok_or("no audio server is running")?;
    let mut server =
        BufReader::new(UnixStream::connect(path).map_err(|error| format!("cannot reach the audio server: {error}"))?);
    let cookie = pulseaudio::cookie_path_from_env().and_then(|path| std::fs::read(path).ok()).unwrap_or_default();
    let login =
        protocol::AuthParams { version: protocol::MAX_VERSION, supports_shm: false, supports_memfd: false, cookie };
    protocol::write_command_message(server.get_mut(), AUTH_TAG, &protocol::Command::Auth(login), protocol::MAX_VERSION)
        .map_err(refused)?;
    let (_, reply) =
        protocol::read_reply_message::<protocol::AuthReply>(&mut server, protocol::MAX_VERSION).map_err(refused)?;
    let version = reply.version.min(protocol::MAX_VERSION);

    let mut properties = protocol::Props::new();
    properties.set(protocol::Prop::ApplicationName, CString::from(c"Remote Desktop"));
    protocol::write_command_message(server.get_mut(), NAME_TAG, &protocol::Command::SetClientName(properties), version)
        .map_err(refused)?;
    protocol::read_reply_message::<protocol::SetClientNameReply>(&mut server, version).map_err(refused)?;
    Ok((server, version))
}

struct Encoder(*mut OpusEncoder);

impl Encoder {
    fn new() -> Result<Self, String> {
        let mut error = 0;
        // "Restricted low delay" drops the codec's look-ahead; the sound follows the picture more closely.
        let raw = unsafe {
            opus_encoder_create(RATE as i32, CHANNELS as i32, OPUS_APPLICATION_RESTRICTED_LOWDELAY, &mut error)
        };
        if raw.is_null() || error != OPUS_OK {
            return Err(format!("cannot create the audio encoder (Opus error {error})"));
        }
        unsafe { opus_encoder_ctl!(raw, OPUS_SET_BITRATE_REQUEST, BITRATE) };
        Ok(Self(raw))
    }

    /// Encodes one frame of interleaved samples into `out` and returns the packet's length.
    fn encode(&mut self, frame: &[i16], out: &mut [u8]) -> Result<usize, String> {
        match unsafe { opus_encode(self.0, frame.as_ptr(), FRAME_SAMPLES as i32, out.as_mut_ptr(), out.len() as i32) } {
            size if size > 0 => Ok(size as usize),
            error => Err(format!("the audio encoder failed (Opus error {error})")),
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { opus_encoder_destroy(self.0) }
    }
}

fn refused(error: protocol::ProtocolError) -> String {
    format!("the audio server refused: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_encodes_to_small_packets() {
        let mut encoder = Encoder::new().unwrap();
        let mut out = [0; LARGEST_PACKET];
        let mut total = 0;
        for index in 0..50 {
            // One second of a 440 Hz tone on both channels, 20 ms at a time.
            let frame: Vec<i16> = (0..FRAME_SAMPLES)
                .flat_map(|i| {
                    let time = (index * FRAME_SAMPLES + i) as f32 / RATE as f32;
                    let sample = ((time * 440.0 * std::f32::consts::TAU).sin() * 12000.0) as i16;
                    [sample, sample]
                })
                .collect();
            total += encoder.encode(&frame, &mut out).unwrap();
        }
        // About 96 kbit/s at most; a pure tone needs less.
        assert!((500..=13_000).contains(&total), "one second took {total} bytes");
    }
}
