//! The X server's screen at the size of the client's window. Its output gets a mode of that size: one of its
//! own where it has one, else one made for it, which a display with no monitor behind it takes and a monitor
//! may; where the server will not have that, the nearest of its own. The mode it had comes back when the
//! client goes.
//!
//! That mode is noted in the server, on the root window, and the host that changed it owns the selection of
//! the same name while it lives. A note without an owner is from a host that ended without putting the mode
//! back, which the next one to start does (see `restore_leftover`); a note with one means another host's
//! client has the screen.
//!
//! Only a server that shows one screen, not rotated, is fitted: with several, the others would have to move.

use std::error::Error;

use x11rb::connection::Connection;
use x11rb::protocol::randr::{self, ConnectionExt as _, ModeFlag, ModeInfo, Rotation, SetConfig};
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, Window as XWindow, WindowClass};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME, NONE};

use crate::screen::{self, Window};

/// What the modes made here are called, each with its size after it, and the note and the selection.
const OWN: &str = crate::NAME;
const NOTE: &[u8] = b"_REMOTE_DESKTOP_MODE";
/// The version of the server's RandR extension that has outputs and their modes.
const OUTPUTS_SINCE: (u32, u32) = (1, 2);
/// The density the screen's size in millimetres is told at, which is what X desktops take for no scaling.
const DOTS_PER_INCH: f64 = 96.0;
const MM_PER_INCH: f64 = 25.4;

type Outcome<T> = Result<T, Box<dyn Error>>;

/// The server's screen while a client has it at its window's size. Dropping it gives the mode back.
pub struct Fitted {
    x: RustConnection,
    root: XWindow,
    note: Atom,
    name: String,
    fps: u32,
    /// The mode the output had, the one it has now, and the one made here, if one is.
    original: randr::Mode,
    applied: randr::Mode,
    made: Option<randr::Mode>,
}

/// The server's one lit output and what drives it, as of now.
struct Lit {
    resources: randr::GetScreenResourcesCurrentReply,
    output: randr::Output,
    info: randr::GetOutputInfoReply,
    crtc: randr::GetCrtcInfoReply,
}

impl Lit {
    fn read(x: &RustConnection, root: XWindow) -> Outcome<Self> {
        let resources = x.randr_get_screen_resources_current(root)?.reply()?;
        let mut lit = Vec::new();
        for &output in &resources.outputs {
            let info = x.randr_get_output_info(output, resources.config_timestamp)?.reply()?;
            if info.crtc != NONE {
                lit.push((output, info));
            }
        }
        let Ok([(output, info)]) = <[_; 1]>::try_from(lit) else {
            return Err("it does not show exactly one screen".into());
        };
        let crtc = x.randr_get_crtc_info(info.crtc, resources.config_timestamp)?.reply()?;
        if crtc.rotation != Rotation::ROTATE0 {
            return Err("its screen is rotated".into());
        }
        Ok(Self { resources, output, info, crtc })
    }

    /// The server's modes, each with its name: the names come in one piece, in the modes' order.
    fn named(&self) -> impl Iterator<Item = (&ModeInfo, &[u8])> {
        let mut names = self.resources.names.as_slice();
        self.resources.modes.iter().map(move |mode| {
            let (name, rest) = names.split_at(usize::from(mode.name_len).min(names.len()));
            names = rest;
            (mode, name)
        })
    }

    /// The output's modes: each with its size and whether it was made here.
    fn modes(&self) -> Vec<(randr::Mode, (u16, u16), bool)> {
        let own = self.named().filter(|(mode, _)| self.info.modes.contains(&mode.id));
        own.map(|(mode, name)| (mode.id, (mode.width, mode.height), name.starts_with(OWN.as_bytes()))).collect()
    }

    fn size_of(&self, mode: randr::Mode) -> Option<(u16, u16)> {
        self.modes().into_iter().find(|&(id, ..)| id == mode).map(|(_, size, _)| size)
    }
}

/// The server, its root window and the note's name, if the server has outputs to give modes.
fn connect() -> Outcome<(RustConnection, XWindow, Atom)> {
    let (x, screen) = x11rb::connect(None)?;
    let version = x.randr_query_version(OUTPUTS_SINCE.0, OUTPUTS_SINCE.1)?.reply()?;
    if (version.major_version, version.minor_version) < OUTPUTS_SINCE {
        return Err("it gives its outputs no modes".into());
    }
    let root = x.setup().roots[screen].root;
    let note = x.intern_atom(false, NOTE)?.reply()?.atom;
    Ok((x, root, note))
}

fn told(error: Box<dyn Error>) -> String {
    format!("the X server: {error}")
}

impl Fitted {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn create(window: Window, fps: u32) -> Result<Self, String> {
        let open = || -> Outcome<Self> {
            let (x, root, note) = connect()?;
            if x.get_selection_owner(note)?.reply()?.owner != NONE {
                return Err("another host's client has its screen".into());
            }
            let lit = Lit::read(&x, root)?;
            let name = String::from_utf8_lossy(&lit.info.name).into_owned();
            let original = lit.crtc.mode;
            // A window of this connection's owns the selection, so the server takes it back with the host.
            let owner = x.generate_id()?;
            let unseen = (COPY_DEPTH_FROM_PARENT, WindowClass::INPUT_ONLY, COPY_FROM_PARENT);
            x.create_window(unseen.0, owner, root, 0, 0, 1, 1, 0, unseen.1, unseen.2, &CreateWindowAux::new())?;
            x.set_selection_owner(owner, note, CURRENT_TIME)?;
            x.change_property32(
                x11rb::protocol::xproto::PropMode::REPLACE,
                root,
                note,
                AtomEnum::CARDINAL,
                &[original],
            )?;
            x.flush()?;
            Ok(Self { x, root, note, name, fps, original, applied: original, made: None })
        };
        // From here on, dropping it undoes what was done, also after a failure halfway.
        let mut fitted = open().map_err(told)?;
        fitted.resize(window)?;
        Ok(fitted)
    }

    /// Gives the screen the size of the client's window as it is now: in the desktop's own units, which an X
    /// desktop has one of to a pixel, and even, as the video needs.
    pub fn resize(&mut self, window: Window) -> Result<(), String> {
        let (width, height) = window.size();
        let even = |length: i32| (length & !1) as u16;
        self.fit((even(width), even(height))).map_err(told)
    }

    fn fit(&mut self, wanted: (u16, u16)) -> Outcome<()> {
        let lit = Lit::read(&self.x, self.root)?;
        if lit.crtc.mode != self.applied {
            return Err("its screen was changed at the machine".into());
        }
        let current = (lit.crtc.width, lit.crtc.height);
        if wanted == current {
            return Ok(());
        }
        let modes = lit.modes();
        let before = self.made;
        let own = modes.iter().find(|&&(_, size, made)| size == wanted && !made).map(|&(mode, ..)| mode);
        let fitted = own.is_some_and(|mode| self.set(mode, wanted)) || self.set_made(&lit, wanted);
        if !fitted {
            let far = |size: (u16, u16)| {
                let wide = |(width, height): (u16, u16)| (u32::from(width), u32::from(height));
                screen::distance(wide(size), wide(wanted))
            };
            let mut sizes: Vec<_> =
                modes.iter().filter(|&&(.., made)| !made).map(|&(mode, size, _)| (mode, size)).collect();
            sizes.sort_by(|a, b| far(a.1).total_cmp(&far(b.1)));
            let nearer = sizes.into_iter().take_while(|&(_, size)| far(size) < far(current));
            let (width, height) =
                nearer.into_iter().find(|&(mode, size)| self.set(mode, size)).map_or(current, |(_, size)| size);
            warn!("{} is {width}x{height} for a client's window of {}x{}", self.name, wanted.0, wanted.1);
        }
        // The mode made for the size before is of no use once the output has another.
        if let Some(mode) = before.filter(|&mode| mode != self.applied) {
            self.unmake(mode);
        }
        Ok(())
    }

    /// Makes a mode of `size`, or finds the one a host before this made, and gives it to the output.
    fn set_made(&mut self, lit: &Lit, size: (u16, u16)) -> bool {
        let name = format!("{OWN} {}x{}@{}", size.0, size.1, self.fps);
        let known = lit.named().filter(|&(_, called)| called == name.as_bytes()).map(|(mode, _)| mode.id).last();
        let made = || -> Outcome<randr::Mode> {
            let timing = ModeInfo { name_len: name.len() as u16, ..timing(size, self.fps) };
            Ok(self.x.randr_create_mode(self.root, timing, name.as_bytes())?.reply()?.mode)
        };
        let Some(mode) = known.or_else(|| made().ok()) else { return false };
        let listed = lit.info.modes.contains(&mode)
            || self.x.randr_add_output_mode(lit.output, mode).is_ok_and(|sent| sent.check().is_ok());
        if listed && self.set(mode, size) {
            self.made = Some(mode);
            return true;
        }
        self.unmake(mode);
        false
    }

    /// Takes a mode made here from the output and from the server, which keeps one that is in use.
    fn unmake(&mut self, mode: randr::Mode) {
        if let Ok(lit) = Lit::read(&self.x, self.root) {
            let _ = self.x.randr_delete_output_mode(lit.output, mode).map(|sent| sent.check());
        }
        let _ = self.x.randr_destroy_mode(mode).map(|sent| sent.check());
        if self.made == Some(mode) {
            self.made = None;
        }
    }

    /// Gives the output `mode`, which is of `size`, and the screen that size. False where the server will not.
    fn set(&mut self, mode: randr::Mode, (width, height): (u16, u16)) -> bool {
        let set = || -> Outcome<bool> {
            let lit = Lit::read(&self.x, self.root)?;
            let (left, top, had) = (lit.crtc.x as u16, lit.crtc.y as u16, (lit.crtc.width, lit.crtc.height));
            let screen = |(width, height): (u16, u16)| {
                let mm = |pixels: u16| (f64::from(pixels) * MM_PER_INCH / DOTS_PER_INCH).round() as u32;
                let (width, height) = (left + width, top + height);
                self.x.randr_set_screen_size(self.root, width, height, mm(width), mm(height))
            };
            // The screen has to hold the output before the change and after it: it is made to hold both
            // first, and nothing else sees the steps between.
            self.x.grab_server()?;
            let stepped = || -> Outcome<bool> {
                screen((width.max(had.0), height.max(had.1)))?.check()?;
                let (at, outputs) = (lit.resources.config_timestamp, &lit.crtc.outputs);
                let change = self.x.randr_set_crtc_config(
                    lit.info.crtc,
                    CURRENT_TIME,
                    at,
                    lit.crtc.x,
                    lit.crtc.y,
                    mode,
                    lit.crtc.rotation,
                    outputs,
                )?;
                let changed = change.reply().is_ok_and(|reply| reply.status == SetConfig::SUCCESS);
                screen(if changed { (width, height) } else { had })?.check()?;
                Ok(changed)
            };
            let changed = stepped();
            self.x.ungrab_server()?;
            self.x.flush()?;
            changed
        };
        let changed = set().unwrap_or(false);
        if changed {
            self.applied = mode;
        }
        changed
    }
}

impl Drop for Fitted {
    fn drop(&mut self) {
        // The mode comes back unless someone at the machine has chosen another since.
        if let Ok(lit) = Lit::read(&self.x, self.root)
            && lit.crtc.mode == self.applied
            && self.applied != self.original
            && !lit.size_of(self.original).is_some_and(|size| self.set(self.original, size))
        {
            warn!("could not give {} its mode back", self.name);
        }
        if let Some(mode) = self.made {
            self.unmake(mode);
        }
        // The answer is waited for: a server may not get to what a connection sent just before it closed.
        let _ = self.x.delete_property(self.root, self.note).map(|sent| sent.check());
    }
}

/// Gives the screen its mode back after a host ended while a client had it, and takes the modes made for
/// clients away. Not while a host that lives has a client on it.
pub fn restore_leftover() {
    let restore = || -> Outcome<()> {
        let (x, root, note) = connect()?;
        let noted = x.get_property(false, root, note, AtomEnum::CARDINAL, 0, 1)?.reply()?;
        let Some(original) = noted.value32().and_then(|mut value| value.next()) else { return Ok(()) };
        if x.get_selection_owner(note)?.reply()?.owner != NONE {
            return Ok(());
        }
        let lit = Lit::read(&x, root)?;
        let (modes, now) = (lit.modes(), lit.crtc.mode);
        let name = String::from_utf8_lossy(&lit.info.name).into_owned();
        // Only a mode made here is a client's: any other, someone at the machine has chosen since.
        let clients = modes.iter().any(|&(mode, _, made)| made && mode == now);
        let original = if clients { original } else { now };
        let mut left = Fitted { x, root, note, name, fps: 0, original, applied: now, made: clients.then_some(now) };
        for (mode, ..) in modes.into_iter().filter(|&(mode, _, made)| made && mode != now) {
            left.unmake(mode);
        }
        // Dropping it puts the mode back, takes the mode made for the client away, and the note with it.
        Ok(())
    };
    if let Err(error) = restore() {
        warn!("could not look for a screen left at a client's size: {error}");
    }
}

/// A mode of `size` at `refresh` pictures a second, timed as VESA's CVT with reduced blanking has it, which is
/// what flat panels are driven with. A display with no monitor behind it takes any timing.
fn timing((width, height): (u16, u16), refresh: u32) -> ModeInfo {
    // Dots across: the blanking in all, and of it the front porch and the sync pulse.
    const ACROSS: (u16, u16, u16) = (160, 48, 32);
    // Lines down: the front porch, the sync pulse for a shape that is none of the standard ones, and the least
    // back porch; and the least time the blanking lasts, in microseconds.
    const DOWN: (u16, u16, u16) = (3, 10, 6);
    const BLANK_MICROSECONDS: f64 = 460.0;
    /// Dot clocks are whole steps of a quarter megahertz.
    const CLOCK_STEP: u64 = 250_000;
    let line = (1e6 / f64::from(refresh) - BLANK_MICROSECONDS) / f64::from(height);
    let blank = ((BLANK_MICROSECONDS / line) as u16 + 1).max(DOWN.0 + DOWN.1 + DOWN.2);
    let (across, down) = (width + ACROSS.0, height + blank);
    let clock = u64::from(refresh) * u64::from(across) * u64::from(down) / CLOCK_STEP * CLOCK_STEP;
    ModeInfo {
        id: 0,
        width,
        height,
        dot_clock: clock as u32,
        hsync_start: width + ACROSS.1,
        hsync_end: width + ACROSS.1 + ACROSS.2,
        htotal: across,
        hskew: 0,
        vsync_start: height + DOWN.0,
        vsync_end: height + DOWN.0 + DOWN.1,
        vtotal: down,
        name_len: 0,
        mode_flags: ModeFlag::HSYNC_POSITIVE | ModeFlag::VSYNC_NEGATIVE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_made_mode_is_timed_as_cvt_with_reduced_blanking() {
        // `cvt -r 1920 1200 60`: 154.00 MHz, 1920 1968 2000 2080, 1200 1203 .. 1235.
        let mode = timing((1920, 1200), 60);
        assert_eq!((mode.dot_clock, mode.hsync_start, mode.hsync_end, mode.htotal), (154_000_000, 1968, 2000, 2080));
        assert_eq!((mode.vsync_start, mode.vtotal), (1203, 1235));
        // `cvt -r 1280 720 60`: 64.00 MHz, .. 1440, .. 741.
        let mode = timing((1280, 720), 60);
        assert_eq!((mode.dot_clock, mode.htotal, mode.vtotal), (64_000_000, 1440, 741));
    }
}
