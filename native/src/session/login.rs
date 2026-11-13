//! What a client says when it logs in: its password, and what it chose on its settings panel. The names and
//! values here are the page's (index.html), which sends the panel's fields as they are.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::capture::Settings;
use crate::platform::virtual_screen::Window;
use crate::rate;

/// How long after a wrong password no login is accepted, which limits guessing to this pace overall.
const GUESS_INTERVAL: Duration = Duration::from_secs(1);
/// The frame rates the panel offers; anything else means the first.
const FRAME_RATES: [u32; 3] = [60, 30, 15];
/// The bitrate a client may ask for at most, in kbit/s.
const CEILING_LIMIT: i64 = 50000;
/// The patience the panel's middle stands for, and the ends of its range, in milliseconds.
const PATIENCE: i64 = 100;
const PATIENCES: (i64, i64) = (25, 400);
/// The panel's values: the pointer painted into the picture, sound, colour detail, size.
const CURSOR_PAINTED: &str = "yes";
const SOUND_ON: &str = "on";
const CHROMA_FULL: &str = "444";
const CHROMA_REDUCED: &str = "420";
/// The panel's sizes: the host's screen as it is, or a screen made to the client's window; anything else lets
/// the stream shrink the host's screen while the link is weak.
const SIZE_CLIENT: &str = "client";
const SIZE_HOST: &str = "host";

/// A login as the client sent it.
pub struct Login {
    pub password: String,
    pub settings: Settings,
    /// The highest bitrate it wants, in kbit/s, and how long it would rather have video fall behind than see
    /// the picture made coarser, in milliseconds.
    pub ceiling: u32,
    pub patience: u32,
    pub sound: bool,
    /// Its window, when it asked for a screen of that size (see platform/linux/virtual_screen.rs).
    pub device: Option<Window>,
}

impl Login {
    /// Reads a login; anything missing or unknown means the default.
    pub fn read(login: &Value) -> Self {
        let ceiling = ceiling(&login["bitrate"]);
        let fps = number(&login["fps"]).and_then(|fps| FRAME_RATES.into_iter().find(|&rate| rate as i64 == fps));
        let chroma = login["chroma"].as_str();
        let size = login["size"].as_str();
        let settings = Settings {
            kbps: rate::start(ceiling),
            fps: fps.unwrap_or(FRAME_RATES[0]),
            cursor: login["cursor"] == CURSOR_PAINTED,
            // Auto begins with full colour detail, which the stream gives up where it does not fit.
            full_chroma: chroma != Some(CHROMA_REDUCED),
            auto_chroma: !matches!(chroma, Some(CHROMA_FULL | CHROMA_REDUCED)),
            height: None,
            auto_size: !matches!(size, Some(SIZE_HOST | SIZE_CLIENT)),
            screen: login["screen"].as_str().filter(|name| !name.is_empty()).map(str::to_owned),
            hevc: login["hevc"] == true,
        };
        Self {
            password: login["password"].as_str().unwrap_or("").to_owned(),
            settings,
            ceiling,
            patience: patience(&login["patience"]),
            sound: login["sound"] == SOUND_ON,
            device: (login["size"] == SIZE_CLIENT).then(|| window(&login["window"])).flatten(),
        }
    }
}

/// A number, sent as one or as text.
fn number(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.parse().ok())
}

/// The client's window as it tells it, at login and when it changes: [width, height, pixel ratio].
pub fn window(told: &Value) -> Option<Window> {
    let number = |index: usize| told.get(index)?.as_f64().filter(|number| number.is_finite() && *number > 0.0);
    Some(Window { width: number(0)?, height: number(1)?, pixel_ratio: number(2)? })
}

/// For how long a client would rather have video fall behind than see the picture made coarser, in
/// milliseconds: from response first to quality first. Nothing means the middle.
pub fn patience(wanted: &Value) -> u32 {
    number(wanted).map_or(PATIENCE, |patience| patience.clamp(PATIENCES.0, PATIENCES.1)) as u32
}

/// The highest bitrate a client asks for, in kbit/s. Nothing or zero leaves the choice to the host.
pub fn ceiling(wanted: &Value) -> u32 {
    match number(wanted) {
        Some(kbps) if kbps > 0 => kbps.clamp(rate::LOWEST.into(), CEILING_LIMIT) as u32,
        _ => rate::HIGHEST,
    }
}

/// Whether a login with this password is let in. After a wrong one, every login is turned away for a moment,
/// the right password included, so that many connections guessing at once gain nothing.
pub fn admitted(offered: &[u8], password: &[u8]) -> bool {
    static LAST_WRONG: Mutex<Option<Instant>> = Mutex::new(None);
    if password.is_empty() {
        return true;
    }
    let mut last_wrong = LAST_WRONG.lock().unwrap();
    if last_wrong.is_some_and(|at| at.elapsed() < GUESS_INTERVAL) {
        return false;
    }
    let right = same(offered, password);
    if !right {
        *last_wrong = Some(Instant::now());
    }
    right
}

/// Compares without stopping at the first difference, so timing does not reveal how much matched.
fn same(offered: &[u8], password: &[u8]) -> bool {
    offered.len() == password.len()
        && offered.iter().zip(password).fold(0, |difference, (a, b)| difference | (a ^ b)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrong_password_closes_the_door_for_a_moment() {
        assert!(admitted(b"", b""), "no password set: everyone is let in");
        assert!(admitted(b"secret", b"secret"));
        assert!(!admitted(b"guess", b"secret"));
        assert!(!admitted(b"secret", b"secret"), "right after a wrong guess even the right password waits");
        std::thread::sleep(GUESS_INTERVAL);
        assert!(admitted(b"secret", b"secret"));
    }

    #[test]
    fn the_ceiling_is_the_clients_choice_within_limits() {
        assert_eq!(ceiling(&serde_json::json!(5000)), 5000);
        assert_eq!(ceiling(&serde_json::json!("5000")), 5000);
        assert_eq!(ceiling(&serde_json::json!(1)), rate::LOWEST);
        assert_eq!(ceiling(&serde_json::json!(900_000)), CEILING_LIMIT as u32);
        for unset in [serde_json::json!(""), serde_json::json!(0), Value::Null] {
            assert_eq!(ceiling(&unset), rate::HIGHEST);
        }
    }

    #[test]
    fn password_comparison() {
        assert!(same(b"secret", b"secret"));
        assert!(!same(b"secreT", b"secret"));
        assert!(!same(b"secre", b"secret"));
        assert!(!same(b"", b"secret"));
    }

    #[test]
    fn a_login_reads_the_panel_and_falls_back_to_defaults() {
        let login = Login::read(&serde_json::json!({
            "password": "secret", "fps": "30", "chroma": "auto", "size": "client", "window": [1512, 982, 2], "sound": "on"
        }));
        assert_eq!((login.password.as_str(), login.settings.fps, login.sound), ("secret", 30, true));
        assert!(login.settings.full_chroma && login.settings.auto_chroma);
        assert_eq!(login.device, Some(Window { width: 1512.0, height: 982.0, pixel_ratio: 2.0 }));
        let defaults = Login::read(&Value::Null);
        assert_eq!((defaults.settings.fps, defaults.patience, defaults.ceiling), (60, 100, rate::HIGHEST));
        assert!(defaults.settings.auto_chroma && defaults.device.is_none() && !defaults.sound);
        assert!(!Login::read(&serde_json::json!({"chroma": "420"})).settings.full_chroma);
        assert!(!Login::read(&serde_json::json!({"size": "host"})).settings.auto_size);
        assert!(!Login::read(&serde_json::json!({"size": "client"})).settings.auto_size);
        assert!(Login::read(&serde_json::json!({"size": "auto"})).settings.auto_size);
        assert!(Login::read(&serde_json::json!({"size": "720"})).settings.auto_size, "an old choice means Auto");
    }
}
