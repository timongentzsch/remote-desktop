//! Fits the selected Windows display to a client's window for the duration of its session.
//! A physical display can use only the resolutions supported by its driver.

mod scale;

use scale::Scale;
use windows::Win32::Foundation::{HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Graphics::Gdi::{
    CDS_TEST, CDS_TYPE, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DM_BITSPERPEL,
    DM_DISPLAYFIXEDOUTPUT, DM_DISPLAYFLAGS, DM_DISPLAYFREQUENCY, DM_DISPLAYORIENTATION, DM_PELSHEIGHT, DM_PELSWIDTH,
    ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE,
};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::{HSTRING, Owned, PCWSTR, w};

use super::{data_directory, display_mode, write_private};
use crate::screen::{self, Window};

/// Names the display a client has and the scale step it had, for a host that ends without giving them back.
fn leftover_path() -> std::path::PathBuf {
    data_directory().join("display")
}

/// Gives a display its own mode and scale back after a host ended while a client had it: killed, as an install
/// does to the host it replaces, or at sign-out. A client's mode is never saved, so the saved one is its own.
pub fn restore_leftover() {
    let Ok(left) = std::fs::read_to_string(leftover_path()) else { return };
    let Ok(_owner) = Owner::take() else { return }; // another host's client has the display now
    let mut left = left.split_whitespace();
    let device = HSTRING::from(format!(r"\\.\{}", left.next().unwrap_or_default()));
    unsafe { ChangeDisplaySettingsExW(PCWSTR(device.as_ptr()), None, None, CDS_TYPE(0), None) };
    if let (Some(step), Some(mut scale)) = (left.next().and_then(|step| step.parse().ok()), Scale::open(&device)) {
        scale.original = step;
        scale.restore();
    }
    let _ = std::fs::remove_file(leftover_path());
}

// A mutex in the interactive Windows session also covers hosts listening on different ports.
struct Owner(Owned<HANDLE>);

impl Owner {
    fn take() -> Result<Self, String> {
        let mutex = unsafe { CreateMutexW(None, false, w!(r"Local\remote-desktop-display-mode")) }
            .map_err(|error| format!("cannot open the display mode lock: {error}"))?;
        let mutex = unsafe { Owned::new(mutex) };
        // Allow a reconnecting viewer's previous connection to finish restoring the desktop.
        match unsafe { WaitForSingleObject(*mutex, 2000) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self(mutex)),
            WAIT_TIMEOUT => Err("another client controls the display size".into()),
            _ => Err(format!("cannot acquire the display mode lock: {}", windows::core::Error::from_thread())),
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _ = unsafe { ReleaseMutex(*self.0) };
    }
}

pub struct VirtualScreen {
    name: String,
    device: HSTRING,
    original: DEVMODEW,
    applied: Option<DEVMODEW>,
    scale: Option<Scale>,
    _owner: Owner,
}

impl VirtualScreen {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn create(window: Window, _fps: u32, wanted: Option<&str>) -> Result<Self, String> {
        let owner = Owner::take()?;
        let screens = super::screens()?;
        let name = screen::choose(&screens, wanted).ok_or("there is no display")?.name.clone();
        let device = HSTRING::from(format!(r"\\.\{name}"));
        let original = display_mode(PCWSTR(device.as_ptr()), ENUM_CURRENT_SETTINGS)
            .ok_or_else(|| format!("cannot read the display mode for {name}"))?;
        let scale = Scale::open(&device);
        let left = scale.as_ref().map_or(name.clone(), |scale| format!("{name} {}", scale.original));
        write_private(&leftover_path(), left.as_bytes())?;
        let mut made = Self { name, device, original, applied: None, scale, _owner: owner };
        made.resize(window)?;
        Ok(made)
    }

    /// The scale Windows currently reports for this display, in percent.
    pub fn scale_percent(&self) -> Option<u32> {
        self.scale.as_ref()?.percent()
    }

    pub fn resize(&mut self, window: Window) -> Result<(), String> {
        // Resizing can change the permitted scales, so observe manual edits before the mode switch
        // and choose the new scale afterwards. This also applies DPR changes at the same resolution.
        let scale_owned = self.scale.as_mut().is_some_and(Scale::owned);
        self.fit_pixels(window.pixels())?;
        if let Some(scale) = &mut self.scale
            && scale_owned
        {
            scale.set(window.pixel_ratio * 100.0);
        }
        Ok(())
    }

    fn fit_pixels(&mut self, wanted: (u32, u32)) -> Result<(), String> {
        let device = PCWSTR(self.device.as_ptr());
        let current =
            display_mode(device, ENUM_CURRENT_SETTINGS).ok_or_else(|| format!("{} disconnected", self.name))?;
        if !same_mode(&current, self.applied.as_ref().unwrap_or(&self.original)) {
            return Err(format!("{} was changed locally", self.name));
        }
        let current = (current.dmPelsWidth, current.dmPelsHeight);
        if wanted == current {
            return Ok(());
        }

        // Some virtual display drivers accept a custom size that they don't enumerate.
        // Keep the original orientation, colour depth, scan and refresh when trying another size.
        let mut mode = self.original;
        mode.dmFields &=
            DM_BITSPERPEL | DM_DISPLAYORIENTATION | DM_DISPLAYFREQUENCY | DM_DISPLAYFLAGS | DM_DISPLAYFIXEDOUTPUT;
        mode.dmFields |= DM_PELSWIDTH | DM_PELSHEIGHT;
        let mut apply = |size: (u32, u32)| {
            (mode.dmPelsWidth, mode.dmPelsHeight) = size;
            let change = |flags| unsafe { ChangeDisplaySettingsExW(device, Some(&mode), None, flags, None) };
            if change(CDS_TEST) != DISP_CHANGE_SUCCESSFUL || change(CDS_TYPE(0)) != DISP_CHANGE_SUCCESSFUL {
                return false;
            }
            self.applied = Some(display_mode(device, ENUM_CURRENT_SETTINGS).unwrap_or(mode));
            true
        };
        if apply(wanted) {
            return Ok(());
        }

        let mut sizes: Vec<_> = (0..)
            .map_while(|index| display_mode(device, ENUM_DISPLAY_SETTINGS_MODE(index)))
            .map(|mode| (mode.dmPelsWidth, mode.dmPelsHeight))
            .collect();
        sizes.sort_unstable();
        sizes.dedup();
        let distance = |size| screen::distance(size, wanted);
        sizes.sort_by(|&a, &b| distance(a).total_cmp(&distance(b)));
        let (width, height) = sizes
            .into_iter()
            .take_while(|&size| distance(size) < distance(current))
            .filter(|&size| size != wanted)
            .find(|&size| apply(size))
            .unwrap_or(current);
        warn!("{} uses {width}x{height} for a {}x{} client window", self.name, wanted.0, wanted.1);
        Ok(())
    }
}

// EnumDisplaySettingsExW fills the display members of these unions. A manual change to the
// resolution, refresh, depth, scan, rotation or layout transfers control back to the user.
fn same_mode(a: &DEVMODEW, b: &DEVMODEW) -> bool {
    let fields = |mode: &DEVMODEW| unsafe {
        (
            mode.dmPelsWidth,
            mode.dmPelsHeight,
            mode.dmBitsPerPel,
            mode.dmDisplayFrequency,
            mode.Anonymous2.dmDisplayFlags,
            mode.Anonymous1.Anonymous2,
        )
    };
    fields(a) == fields(b)
}

impl Drop for VirtualScreen {
    fn drop(&mut self) {
        let device = PCWSTR(self.device.as_ptr());
        let scale_owned = self.scale.as_mut().is_some_and(Scale::owned);
        if let Some(scale) = &mut self.scale
            && scale_owned
        {
            scale.restore();
        }
        if let Some(applied) = &self.applied
            && !same_mode(applied, &self.original)
            && display_mode(device, ENUM_CURRENT_SETTINGS).is_some_and(|current| same_mode(&current, applied))
        {
            let result = unsafe { ChangeDisplaySettingsExW(device, Some(&self.original), None, CDS_TYPE(0), None) };
            if result != DISP_CHANGE_SUCCESSFUL {
                warn!("could not restore the display mode of {} (Windows {})", self.name, result.0);
            }
            // A lower resolution may have hidden the original scale until the original mode returned.
            if let Some(scale) = &mut self.scale
                && scale_owned
            {
                scale.restore();
            }
        }
        let _ = std::fs::remove_file(leftover_path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_refresh_or_size_manually_relinquishes_restore() {
        let applied = DEVMODEW { dmPelsWidth: 1280, dmPelsHeight: 800, dmDisplayFrequency: 60, ..Default::default() };
        assert!(same_mode(&applied, &applied));
        assert!(!same_mode(&applied, &DEVMODEW { dmDisplayFrequency: 120, ..applied }));
        assert!(!same_mode(&applied, &DEVMODEW { dmPelsHeight: 720, ..applied }));
    }
}
