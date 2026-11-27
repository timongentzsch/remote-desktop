//! The Mac's sound, recorded through ScreenCaptureKit by sound.m. While nothing plays, silence arrives, which
//! audio.rs does not send.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::audio::{CHANNELS, FRAME_SAMPLES, Sound};
use crate::capture::Shared;

const ERROR_SIZE: usize = 256;
/// How long the sound thread waits for samples before it looks whether the session has ended.
const LOOK: c_int = 250;

unsafe extern "C" {
    fn sound_start(error: *mut c_char, size: c_int) -> *mut c_void;
    fn sound_take(sound: *mut c_void, out: *mut i16, capacity: c_int, millis: c_int) -> c_int;
    fn sound_stop(sound: *mut c_void);
}

/// Records until `shared.stop` is set and hands the samples to `sound`. The recording goes with a display and
/// stops when that one goes, as the virtual display does when it is made anew: it is begun again then.
pub fn run(shared: Arc<Shared>, sound: &mut Sound) -> Result<(), String> {
    let mut samples = vec![0i16; FRAME_SAMPLES * CHANNELS * 4];
    while !shared.stop.load(Ordering::Relaxed) {
        let mut error = [0 as c_char; ERROR_SIZE];
        let recording = unsafe { sound_start(error.as_mut_ptr(), ERROR_SIZE as c_int) };
        if recording.is_null() {
            let reason = unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy();
            return Err(format!("the sound cannot be recorded: {reason}"));
        }
        let mut heard = Ok(());
        while heard.is_ok() && !shared.stop.load(Ordering::Relaxed) {
            match unsafe { sound_take(recording, samples.as_mut_ptr(), samples.len() as c_int, LOOK) } {
                -1 => break,
                count => heard = sound.take(samples[..count as usize].iter().copied()),
            }
        }
        unsafe { sound_stop(recording) };
        heard?;
    }
    Ok(())
}
