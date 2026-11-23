//! The desktop's sound, recorded from the default output through WASAPI's loopback, which Windows converts to
//! 48 kHz stereo 16-bit for it. While nothing plays, nothing arrives.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioCaptureClient, IAudioClient,
    IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, eConsole, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};

use crate::audio::{CHANNELS, RATE, Sound};
use crate::capture::Shared;

/// WAVE_FORMAT_PCM, integer samples.
const PCM: u16 = 1;
/// How much the loopback holds, in units of 100 ns, and how often it is emptied.
const BUFFERED: i64 = 2_000_000;
const LOOK: Duration = Duration::from_millis(10);

/// Records until `shared.stop` is set and hands the samples to `sound`.
pub fn run(shared: Arc<Shared>, sound: &mut Sound) -> Result<(), String> {
    let failed = |error: windows::core::Error| format!("the sound cannot be recorded: {error}");
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok().map_err(failed)?;
    let devices: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(failed)?;
    let output = unsafe { devices.GetDefaultAudioEndpoint(eRender, eConsole) }.map_err(failed)?;
    let client: IAudioClient = unsafe { output.Activate(CLSCTX_ALL, None) }.map_err(failed)?;
    let block = (CHANNELS * 2) as u16;
    let format = WAVEFORMATEX {
        wFormatTag: PCM,
        nChannels: CHANNELS as u16,
        nSamplesPerSec: RATE,
        nAvgBytesPerSec: RATE * block as u32,
        nBlockAlign: block,
        wBitsPerSample: 16,
        cbSize: 0,
    };
    let flags =
        AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFERED, 0, &format, None) }.map_err(failed)?;
    let capture: IAudioCaptureClient = unsafe { client.GetService() }.map_err(failed)?;
    unsafe { client.Start() }.map_err(failed)?;
    while !shared.stop.load(Ordering::Relaxed) {
        std::thread::sleep(LOOK);
        while unsafe { capture.GetNextPacketSize() }.map_err(failed)? > 0 {
            let (mut data, mut frames, mut state) = (std::ptr::null_mut(), 0, 0);
            unsafe { capture.GetBuffer(&mut data, &mut frames, &mut state, None, None) }.map_err(failed)?;
            let count = frames as usize * CHANNELS;
            let samples = unsafe { std::slice::from_raw_parts(data.cast::<i16>(), count) };
            let silent = state & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            let taken =
                if silent { sound.take(std::iter::repeat_n(0, count)) } else { sound.take(samples.iter().copied()) };
            unsafe { capture.ReleaseBuffer(frames) }.map_err(failed)?;
            taken?;
        }
    }
    let _ = unsafe { client.Stop() };
    Ok(())
}
