//! The desktop's sound, recorded from the audio server's monitor of the default output.
//!
//! It speaks the PulseAudio protocol, which PipeWire's and PulseAudio's servers both answer.

use std::ffi::CString;
use std::io::{BufReader, Read};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use pulseaudio::protocol;

use crate::audio::{CHANNELS, FRAME_BYTES, RATE, Sound};
use crate::capture::Shared;

/// The sound server matches each reply to its command by a tag; these commands are sent one at a time.
const AUTH_TAG: u32 = 0;
const NAME_TAG: u32 = 1;
const RECORD_TAG: u32 = 2;
/// How often the sound thread looks whether the session has ended, while the sound server sends nothing.
const LOOK: std::time::Duration = std::time::Duration::from_millis(250);

/// Records until `shared.stop` is set and hands the samples to `sound`.
pub fn run(shared: Arc<Shared>, sound: &mut Sound) -> Result<(), String> {
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

    let mut chunk = Vec::new();
    while !shared.stop.load(Ordering::Relaxed) {
        if server.buffer().is_empty()
            && crate::platform::unix::fd::wait_readable([server.get_ref().as_raw_fd()], Some(LOOK)) == [false]
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
        sound.take(chunk.as_chunks::<2>().0.iter().map(|&sample| i16::from_le_bytes(sample)))?;
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

fn refused(error: protocol::ProtocolError) -> String {
    format!("the audio server refused: {error}")
}
