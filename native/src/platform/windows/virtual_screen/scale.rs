//! DisplayConfig's per-source scale packets used by Windows Settings. The SDK omits the two
//! packet types; their C layouts and scale steps are in Wine's include/wingdi.h
//! (DISPLAYCONFIG_GET_SOURCE_DPI_SCALE and DISPLAYCONFIG_SET_SOURCE_DPI_SCALE).

use windows::Wdk::Graphics::Direct3D::{
    D3DKMT_CLOSEADAPTER, D3DKMT_OPENADAPTERFROMGDIDISPLAYNAME, D3DKMTCloseAdapter, D3DKMTOpenAdapterFromGdiDisplayName,
};
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_DEVICE_INFO_TYPE, DisplayConfigGetDeviceInfo,
    DisplayConfigSetDeviceInfo,
};
use windows::Win32::Foundation::LUID;
use windows::core::HSTRING;

const PERCENTS: [u32; 12] = [100, 125, 150, 175, 200, 225, 250, 300, 350, 400, 450, 500];

#[repr(C)]
struct Packet<const N: usize> {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    steps: [i32; N],
}

#[derive(Clone, Copy)]
struct Source {
    adapter: LUID,
    id: u32,
}

impl Source {
    fn packet<const N: usize>(self, kind: i32, steps: [i32; N]) -> Packet<N> {
        Packet {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_TYPE(kind),
                size: size_of::<Packet<N>>() as u32,
                adapterId: self.adapter,
                id: self.id,
            },
            steps,
        }
    }

    fn steps(self) -> Option<Steps> {
        let mut packet = self.packet(-3, [0; 3]);
        // Pass a pointer derived from the *whole* packet: the call also writes beyond its header.
        let result = unsafe { DisplayConfigGetDeviceInfo((&raw mut packet).cast()) };
        (result == 0).then(|| Steps::read(packet.steps)).flatten()
    }
}

/// Windows keeps the configured step even if a smaller mode lowers the current maximum.
/// The effective DPI is clamped, but the stored preference still matters on the next resize.
#[derive(Clone, Copy)]
struct Steps {
    min: i32,
    current: i32,
    max: i32,
}

impl Steps {
    fn read([min, current, max]: [i32; 3]) -> Option<Self> {
        let last = PERCENTS.len() as i32 - 1;
        // A custom DPI can return a sentinel instead of a preset step. Leave it alone.
        (min <= 0 && max >= 0 && max.checked_sub(min)? <= last && (-last..=last).contains(&current)).then_some(Self {
            min,
            current,
            max,
        })
    }

    fn percent(self) -> u32 {
        PERCENTS[(self.current.clamp(self.min, self.max) - self.min) as usize]
    }

    fn closest(self, percent: f64) -> i32 {
        let percent = percent.clamp(100.0, 500.0);
        let error = |step: i32| (f64::from(PERCENTS[(step - self.min) as usize]) - percent).abs();
        (self.min..=self.max).min_by(|&a, &b| error(a).total_cmp(&error(b))).unwrap()
    }

    /// The stored preference in a range with a different recommended (zero) step.
    fn relative_to(self, range: Self) -> i32 {
        self.current - self.min + range.min
    }

    fn same_preference(self, other: Self) -> bool {
        self.current == other.current || self.relative_to(other) == other.current
    }
}

pub(super) struct Scale {
    source: Source,
    /// The step to return to, counted from the smallest, which is the same step in every mode.
    pub original: i32,
    // None after a manual change. Comparing the raw preference distinguishes it from a mode clamp.
    expected: Option<Steps>,
}

impl Scale {
    pub fn open(device: &HSTRING) -> Option<Self> {
        let mut adapter = D3DKMT_OPENADAPTERFROMGDIDISPLAYNAME::default();
        if device.len() >= adapter.DeviceName.len() {
            return None;
        }
        adapter.DeviceName[..device.len()].copy_from_slice(device);
        if unsafe { D3DKMTOpenAdapterFromGdiDisplayName(&mut adapter) }.is_err() {
            return None;
        }
        let _ = unsafe { D3DKMTCloseAdapter(&D3DKMT_CLOSEADAPTER { hAdapter: adapter.hAdapter }) };
        let source = Source { adapter: adapter.AdapterLuid, id: adapter.VidPnSourceId };
        let original = source.steps()?;
        Some(Self { source, original: original.current - original.min, expected: Some(original) })
    }

    pub fn percent(&self) -> Option<u32> {
        Some(self.source.steps()?.percent())
    }

    pub fn owned(&mut self) -> bool {
        let Some(current) = self.source.steps() else { return false };
        if self.expected.is_some_and(|expected| !expected.same_preference(current)) {
            self.expected = None;
        }
        self.expected.is_some()
    }

    pub fn set(&mut self, percent: f64) {
        if let Some(steps) = self.source.steps() {
            self.apply(steps, steps.closest(percent));
        }
    }

    pub fn restore(&mut self) {
        if let Some(steps) = self.source.steps() {
            let original = self.original.saturating_add(steps.min);
            // Restore the underlying preference verbatim when Windows permits it. A subsequent
            // read after restoring the original resolution can expose more supported steps.
            if !self.apply(steps, original) && !(steps.min..=steps.max).contains(&original) {
                self.apply(steps, original.clamp(steps.min, steps.max));
            }
        }
    }

    fn apply(&mut self, steps: Steps, relative: i32) -> bool {
        self.expected = Some(steps);
        if steps.current == relative {
            return true;
        }
        let packet = self.source.packet(-4, [relative]);
        let result = unsafe { DisplayConfigSetDeviceInfo((&raw const packet).cast()) };
        if result == 0 {
            self.expected = Some(Steps { current: relative, ..steps });
            return true;
        }
        warn!("Windows kept the display scale at {}% (error {result})", steps.percent());
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_packets_have_the_native_header_and_signed_step_layout() {
        assert_eq!(std::mem::offset_of!(Packet<3>, steps), 20);
        assert_eq!(size_of::<Packet<3>>(), 32);
        assert_eq!(size_of::<Packet<1>>(), 24);
    }

    #[test]
    fn a_mode_clamp_keeps_the_stored_preference_and_allows_an_explicit_lower_scale() {
        let large = Steps::read([0, 4, 4]).unwrap();
        let small = Steps::read([0, 4, 1]).unwrap();
        assert_eq!(large.percent(), 200);
        assert_eq!(small.percent(), 125);
        assert!(large.same_preference(small));
        assert_ne!(small.closest(125.0), small.current, "125% still needs an explicit write");
        assert_eq!(small.closest(100.0), 0);
        assert_eq!(small.relative_to(large), 4, "restore the saved 200% preference at the larger resolution");
        assert!(!small.same_preference(Steps::read([0, 1, 1]).unwrap()), "a manual change takes ownership");
    }

    #[test]
    fn maps_relative_ranges_and_clamps_to_the_supported_steps() {
        let steps = Steps::read([-2, -1, 4]).unwrap();
        assert_eq!(steps.percent(), 125);
        assert_eq!(steps.closest(200.0), 2);
        assert_eq!(steps.closest(139.0), 0);
        assert_eq!(steps.closest(350.0), 4);
        assert_eq!(Steps::read([0, 0, 0]).unwrap().closest(200.0), 0);
        assert!(Steps::read([0, 1000, 2]).is_none());
        assert!(Steps::read([i32::MIN, 0, 2]).is_none());
    }
}
