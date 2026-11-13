//! The desktop's sound as it goes to the client: 48 kHz stereo, encoded as Opus in packets of 20 ms. What
//! records it is the platform's (see `platform::record_sound`); silence is not sent.

use unsafe_libopus::{
    OPUS_APPLICATION_RESTRICTED_LOWDELAY, OPUS_OK, OPUS_SET_BITRATE_REQUEST, OpusEncoder, opus_encode,
    opus_encoder_create, opus_encoder_ctl, opus_encoder_destroy,
};

/// The sound as it is recorded: samples a second, and channels, interleaved as 16-bit integers. The client
/// (index.html) plays it at the same.
pub const RATE: u32 = 48000;
pub const CHANNELS: usize = 2;
/// 20 ms, the usual Opus frame, in samples of each channel and in bytes.
pub const FRAME_SAMPLES: usize = RATE as usize / 50;
pub const FRAME_BYTES: usize = FRAME_SAMPLES * CHANNELS * 2;
const BITRATE: i32 = 96_000;
/// The most an encoded packet of sound may take, in bytes, as Opus recommends.
const LARGEST_PACKET: usize = 4000;

/// Receives each encoded packet of sound.
pub type Packets = Box<dyn FnMut(&[u8]) + Send>;

/// Turns recorded samples into packets: cuts them into frames, leaves out the silent ones and encodes the rest.
pub struct Sound {
    encoder: Encoder,
    samples: Vec<i16>,
    encoded: [u8; LARGEST_PACKET],
    packet: Packets,
}

impl Sound {
    pub fn new(packet: Packets) -> Result<Self, String> {
        Ok(Self { encoder: Encoder::new()?, samples: Vec::new(), encoded: [0; LARGEST_PACKET], packet })
    }

    /// Takes the next interleaved samples, as many as there are, and sends each frame that is complete.
    pub fn take(&mut self, samples: impl IntoIterator<Item = i16>) -> Result<(), String> {
        self.samples.extend(samples);
        while self.samples.len() >= FRAME_SAMPLES * CHANNELS {
            let frame: Vec<i16> = self.samples.drain(..FRAME_SAMPLES * CHANNELS).collect();
            if frame.iter().any(|&sample| sample != 0) {
                let size = self.encoder.encode(&frame, &mut self.encoded)?;
                (self.packet)(&self.encoded[..size]);
            }
        }
        Ok(())
    }
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
