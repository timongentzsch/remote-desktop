//! Windows Graphics Capture leaves the pointer out of the video. Send its shape, or an empty one
//! while it is not shown, so moving it on the client never waits for a frame.

use windows::Win32::Graphics::Gdi::{
    BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, GdiFlush,
    GetObjectW, HMONITOR, MONITOR_DEFAULTTONULL, MonitorFromPoint, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, GetCursorInfo, GetIconInfo, HCURSOR, ICONINFO,
};
use windows::core::Owned;

use crate::capture::Shape;
use crate::stream::Stream;

#[derive(Default)]
pub(super) struct LocalCursor {
    handle: HCURSOR,
    shape: Option<Shape>,
}

impl LocalCursor {
    pub(super) fn update(&mut self, stream: &mut Stream, monitor: HMONITOR) {
        let Ok(current) = current(monitor) else { return };
        // A pointer that is hidden or on another display leaves the client none to draw.
        let Some(cursor) = current else { return stream.pointer(&Shape::default()) };
        if cursor != self.handle
            && let Some(shape) = shape(cursor)
        {
            (self.handle, self.shape) = (cursor, Some(shape));
        }
        if let Some(shape) = &self.shape {
            stream.pointer(shape);
        }
    }
}

/// A visible system pointer with no separate DXGI pointer may be in the frame. Prefer cursor-free capture
/// when Windows cannot report its position.
pub(super) fn on_monitor(monitor: HMONITOR) -> bool {
    !matches!(current(monitor), Ok(None))
}

fn current(monitor: HMONITOR) -> windows::core::Result<Option<HCURSOR>> {
    let mut info = CURSORINFO { cbSize: size_of::<CURSORINFO>() as u32, ..Default::default() };
    unsafe { GetCursorInfo(&mut info) }?;
    let visible = info.flags == CURSOR_SHOWING
        && !info.hCursor.is_invalid()
        && unsafe { MonitorFromPoint(info.ptScreenPos, MONITOR_DEFAULTTONULL) } == monitor;
    Ok(visible.then_some(info.hCursor))
}

fn shape(cursor: HCURSOR) -> Option<Shape> {
    let icon = cursor.into();
    let mut info = ICONINFO::default();
    unsafe { GetIconInfo(icon, &mut info) }.ok()?;
    // GetIconInfo gives the caller two new bitmaps. Monochrome cursors have both masks in one bitmap.
    let mask = unsafe { Owned::new(info.hbmMask) };
    let colour = unsafe { Owned::new(info.hbmColor) };
    let mut size = BITMAP::default();
    if unsafe { GetObjectW((*mask).into(), size_of::<BITMAP>() as i32, Some((&raw mut size).cast())) } == 0 {
        return None;
    }
    let width = u16::try_from(size.bmWidth).ok()?;
    let height = u16::try_from(if colour.is_invalid() { size.bmHeight / 2 } else { size.bmHeight }).ok()?;
    if width == 0 || height == 0 {
        return None;
    }

    // Drawing twice also handles the AND/XOR masks used by monochrome and older colour cursors.
    let header = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: 2 * i32::from(width),
        biHeight: -i32::from(height), // rows from the top
        biPlanes: 1,
        biBitCount: 32,
        ..Default::default()
    };
    let format = BITMAPINFO { bmiHeader: header, ..Default::default() };
    let mut bits = std::ptr::null_mut();
    let bitmap = unsafe { Owned::new(CreateDIBSection(None, &format, DIB_RGB_COLORS, &mut bits, None, 0).ok()?) };
    let columns = usize::from(width);
    let count = 2 * columns * usize::from(height);
    let pixels = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u32>(), count) };
    for row in pixels.chunks_exact_mut(2 * columns) {
        row[..columns].fill(0);
        row[columns..].fill(0x00ff_ffff);
    }

    let dc = unsafe { CreateCompatibleDC(None) };
    if dc.is_invalid() {
        return None;
    }
    let draw = |x| unsafe { DrawIconEx(dc, x, 0, icon, width.into(), height.into(), 0, None, DI_NORMAL) };
    let drawn = unsafe { !SelectObject(dc, (*bitmap).into()).is_invalid() }
        && draw(0).is_ok()
        && draw(width.into()).is_ok()
        && unsafe { GdiFlush().as_bool() };
    let _ = unsafe { DeleteDC(dc) };
    if !drawn {
        return None;
    }
    let pixels = unsafe { std::slice::from_raw_parts(bits.cast::<u32>(), count) };
    let argb = pixels.chunks_exact(2 * columns).flat_map(|row| {
        let (dark, light) = row.split_at(columns);
        dark.iter().zip(light).map(|(&dark, &light)| from_backgrounds(dark, light))
    });
    Some(Shape::from_premultiplied(width, height, (info.xHotspot as u16, info.yHotspot as u16), argb))
}

/// The colour over black is premultiplied. White adds the uncovered fraction of the background.
/// A cursor that inverts a background pixel is represented as opaque black.
fn from_backgrounds(dark: u32, light: u32) -> u32 {
    let (a, b) = (dark.to_le_bytes(), light.to_le_bytes());
    if (0..3).any(|i| a[i] > b[i]) {
        return 0xff00_0000;
    }
    (dark & 0x00ff_ffff) | (u32::from(255 - (b[0] - a[0])) << 24)
}

#[cfg(test)]
mod tests {
    use super::{from_backgrounds, shape};
    use windows::Win32::UI::WindowsAndMessaging::{IDC_ARROW, IDC_IBEAM, LoadCursorW};

    #[test]
    fn reads_windows_arrow_and_text_cursor_at_their_hotspots() {
        for id in [IDC_ARROW, IDC_IBEAM] {
            let cursor = shape(unsafe { LoadCursorW(None, id) }.unwrap()).unwrap();
            assert!(cursor.hot.0 < cursor.width && cursor.hot.1 < cursor.height);
            let (pixels, _) = cursor.pixels.as_chunks::<4>();
            assert!(pixels.iter().any(|p| p[3] == 0));
            assert!(pixels.iter().any(|p| p[3] != 0));
        }
    }

    #[test]
    fn reconstructs_opaque_translucent_transparent_and_inverting_pixels() {
        assert_eq!(from_backgrounds(0x00a3_2751, 0x00a3_2751), 0xffa3_2751);
        assert_eq!(from_backgrounds(0x0030_1122, 0x00af_90a1), 0x8030_1122);
        assert_eq!(from_backgrounds(0, 0x00ff_ffff), 0);
        assert_eq!(from_backgrounds(0x00ff_ffff, 0), 0xff00_0000);
    }
}
