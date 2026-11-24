//! The screen's picture through Desktop Duplication: Windows hands over each new frame of the chosen display, as
//! blue, green, red and alpha, and none while the screen is still. When Windows supplies a separate pointer, we
//! paint it into the frame or send its shape to the client. Graphics Capture can omit a baked-in pointer
//! for a client that draws its own. A display that changes mode is captured anew.

use std::time::{Duration, Instant};

mod cursor;
mod graphics;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_ROTATE90, DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_POINTER_SHAPE_INFO,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR, IDXGIOutput, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource,
};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::core::Interface;

use super::outputs;
use crate::capture::Shape;
use crate::screen::Screen;
use crate::stream::Stream;

/// How often to look for a newly attached or preferred display. Mode changes also wake Desktop Duplication.
const CHECK_DISPLAYS: Duration = Duration::from_millis(250);

/// Stay on the frame clock through rounding in DXGI's millisecond waits. After a missed interval or idle
/// desktop, begin a new interval at capture time so there is no burst of overdue frames.
fn next_frame(due: Instant, started: Instant, interval: Duration) -> Instant {
    if started >= due + interval { started + interval } else { due + interval }
}

/// The pointer as Desktop Duplication tells it: its shape, as pixels with alpha whose colours are multiplied by
/// it, and where its top left corner is on the display, if it is shown there.
#[derive(Default)]
struct Pointer {
    width: usize,
    pixels: Vec<u32>,
    at: Option<(i32, i32)>,
    initialized: bool,
}

impl Pointer {
    /// Only mouse updates carry a valid position. Report visibility on the first frame too: a driver that
    /// draws the pointer into the image may never report a separate pointer.
    fn update_position(&mut self, info: &DXGI_OUTDUPL_FRAME_INFO) -> Option<bool> {
        let was_visible = self.at.is_some();
        if info.LastMouseUpdateTime != 0 {
            let position = info.PointerPosition;
            self.at = position.Visible.as_bool().then_some((position.Position.x, position.Position.y));
        }
        let visible = self.at.is_some();
        let changed = !self.initialized || was_visible != visible;
        self.initialized = true;
        changed.then_some(visible)
    }
}

/// Says what of the capture failed, and how.
fn failed(what: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |error| format!("the screen cannot be captured: {what}: {error}")
}

/// Captures the chosen display until another is chosen, its mode changes, or the stream is no longer wanted.
pub fn run(stream: &mut Stream) -> Result<(), String> {
    let outputs = outputs().map_err(failed("no display"))?;
    let names: Vec<_> = outputs.iter().map(|(screen, ..)| screen.clone()).collect();
    let chosen = crate::screen::choose(&names, stream.settings.screen.as_deref()).ok_or("there is no display")?;
    let (screen, adapter, output) = outputs.into_iter().find(|(screen, ..)| screen == chosen).unwrap();

    let (mut device, mut context) = (None::<ID3D11Device>, None::<ID3D11DeviceContext>);
    unsafe {
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(failed("no Direct3D device"))?;
    let (device, context) = (device.unwrap(), context.unwrap());
    if !stream.settings.cursor {
        // Keep Desktop Duplication when it supplies a separate hardware cursor. A software cursor can
        // already be baked into its surface. Graphics Capture can explicitly exclude that cursor.
        let monitor = unsafe { output.GetDesc() }.map_err(failed("the output"))?.Monitor;
        match duplicate(stream, &screen, &output, &device, &context, Some(monitor)) {
            Ok(End::Reopen) => return Ok(()),
            Ok(End::Embedded) => {}
            Err(error) => warn!("{error}; trying Windows Graphics Capture"),
        }
        match graphics::run(stream, &screen, monitor, &device, &context) {
            Ok(()) => return Ok(()),
            Err(error) => warn!("cursor-free capture unavailable: {error}; using Desktop Duplication"),
        }
    }
    duplicate(stream, &screen, &output, &device, &context, None).map(|_| ())
}

enum End {
    Reopen,
    Embedded,
}

fn duplicate(
    stream: &mut Stream,
    screen: &Screen,
    output: &IDXGIOutput,
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    local_monitor: Option<HMONITOR>,
) -> Result<End, String> {
    let duplication = unsafe { output.cast::<IDXGIOutput1>().and_then(|output| output.DuplicateOutput(device)) }
        .map_err(failed("Desktop Duplication refused"))?;
    let rotation = unsafe { duplication.GetDesc() }.Rotation;

    // The latest picture as it came, and the copy handed on, with the pointer painted in where it is wanted.
    let mut picture = Picture::default();
    let mut shown = Vec::new();
    let mut staging = None;
    let mut pointer = Pointer::default();
    let mut stale = false;
    let mut due = Instant::now();
    let mut check_displays = due + CHECK_DISPLAYS;
    while stream.wanted() {
        let now = Instant::now();
        if now >= check_displays {
            if !stream.same_display(screen)? {
                return Ok(End::Reopen);
            }
            check_displays = now + CHECK_DISPLAYS;
        }
        let mut wait = stream.patience().min(check_displays.saturating_duration_since(now));
        if stale {
            wait = wait.min(due.saturating_duration_since(now));
        }
        let millis = wait.as_nanos().div_ceil(1_000_000) as u32;
        let (mut info, mut resource) = (DXGI_OUTDUPL_FRAME_INFO::default(), None::<IDXGIResource>);
        match unsafe { duplication.AcquireNextFrame(millis, &mut info, &mut resource) } {
            Ok(()) => {
                let visible = pointer.update_position(&info);
                if pointer.at.is_none() && local_monitor.is_some_and(cursor::on_monitor) {
                    unsafe { duplication.ReleaseFrame() }.map_err(failed("the frame"))?;
                    return Ok(End::Embedded);
                }
                let desktop = resource.filter(|_| info.LastPresentTime != 0);
                let taken = desktop
                    .map(|resource| {
                        let texture = resource.cast()?;
                        take(context, device, &texture, &mut staging, rotation, &mut picture)
                    })
                    .transpose();
                let shape = new_shape(&duplication, &info, &mut pointer);
                unsafe { duplication.ReleaseFrame() }.map_err(failed("the frame"))?;
                taken.map_err(failed("the frame"))?;
                if !stream.settings.cursor {
                    if let Some(shape) = shape {
                        stream.pointer(shape);
                    }
                    if let Some(visible) = visible {
                        stream.pointer_visible(visible);
                    }
                }
                stale |= info.LastPresentTime != 0 || (stream.settings.cursor && info.LastMouseUpdateTime != 0);
            }
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => {}
            Err(error) if error.code() == DXGI_ERROR_ACCESS_LOST => return Ok(End::Reopen),
            Err(error) => return Err(failed("the next frame")(error)),
        }
        let started = Instant::now();
        if stale && !picture.bytes.is_empty() && started >= due {
            due = next_frame(due, started, stream.frame_interval());
            let (width, height) = picture.size;
            let bytes = if stream.settings.cursor && pointer.at.is_some() && !pointer.pixels.is_empty() {
                // DXGI already reports the separate pointer in desktop orientation.
                shown.clone_from(&picture.bytes);
                paint(&pointer, &mut shown, width, height);
                &shown
            } else {
                &picture.bytes
            };
            stream.picture(bytes, width * 4, width, height);
            stale = false;
        }
        stale |= stream.refine();
    }
    Ok(End::Reopen)
}

#[derive(Default)]
struct Picture {
    bytes: Vec<u8>,
    size: (usize, usize),
}

impl Picture {
    /// The texture is in the panel's native orientation. Copy into desktop orientation directly from the
    /// padded staging rows. DXGI rotations are clockwise and yuv's rotations are counter-clockwise.
    fn copy(
        &mut self,
        src: &[u8],
        pitch: usize,
        width: usize,
        height: usize,
        rotation: DXGI_MODE_ROTATION,
    ) -> Result<(), yuv::YuvError> {
        use yuv::RotationMode::*;
        self.bytes.resize(width * height * 4, 0);
        let mode = match rotation {
            DXGI_MODE_ROTATION_ROTATE90 => Rotate270,
            DXGI_MODE_ROTATION_ROTATE180 => Rotate180,
            DXGI_MODE_ROTATION_ROTATE270 => Rotate90,
            _ => {
                self.size = (width, height);
                for (dst, row) in self.bytes.chunks_exact_mut(width * 4).zip(src.chunks_exact(pitch)) {
                    dst.copy_from_slice(&row[..width * 4]);
                }
                return Ok(());
            }
        };
        self.size = if mode == Rotate180 { (width, height) } else { (height, width) };
        yuv::rotate_rgba(src, pitch, &mut self.bytes, self.size.0 * 4, width, height, mode)
    }
}

/// Copies a new picture, where the frame has one, into `picture`, through a texture the processor can read.
fn take(
    context: &ID3D11DeviceContext,
    device: &ID3D11Device,
    texture: &ID3D11Texture2D,
    staging: &mut Option<ID3D11Texture2D>,
    rotation: DXGI_MODE_ROTATION,
    picture: &mut Picture,
) -> windows::core::Result<()> {
    let mut description = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut description) };
    if staging.is_none() {
        let readable = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
            ..description
        };
        unsafe { device.CreateTexture2D(&readable, None, Some(staging)) }?;
    }
    let staging = staging.as_ref().unwrap();
    unsafe { context.CopyResource(staging, texture) };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe { context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }?;
    let (width, height) = (description.Width as usize, description.Height as usize);
    let pitch = mapped.RowPitch as usize;
    let rows = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch * height) };
    let copied = picture.copy(rows, pitch, width, height, rotation);
    unsafe { context.Unmap(staging, 0) };
    copied.map_err(|error| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, error.to_string()))
}

/// Reads the pointer's new shape, where the frame has one, into `pointer`, and gives it as the client is told it.
fn new_shape(
    duplication: &IDXGIOutputDuplication,
    info: &DXGI_OUTDUPL_FRAME_INFO,
    pointer: &mut Pointer,
) -> Option<Shape> {
    if info.PointerShapeBufferSize == 0 {
        return None;
    }
    let mut buffer = vec![0u8; info.PointerShapeBufferSize as usize];
    let (mut size, mut shape) = (0, DXGI_OUTDUPL_POINTER_SHAPE_INFO::default());
    unsafe { duplication.GetFramePointerShape(buffer.len() as u32, buffer.as_mut_ptr().cast(), &mut size, &mut shape) }
        .ok()?;
    let (width, height, pixels) = premultiplied(&shape, &buffer);
    let hot = (shape.HotSpot.x as u16, shape.HotSpot.y as u16);
    pointer.width = width;
    pointer.pixels = pixels;
    Some(Shape::from_premultiplied(width as u16, height as u16, hot, pointer.pixels.iter().copied()))
}

/// A pointer's shape as pixels with alpha, the colours multiplied by it. Windows has three kinds: colour, colour
/// with a mask, and black and white. A shape that inverts what is under it, as the text cursor does, cannot be
/// told as colours: it is black here, and so is a masked colour pixel that inverts.
fn premultiplied(shape: &DXGI_OUTDUPL_POINTER_SHAPE_INFO, buffer: &[u8]) -> (usize, usize, Vec<u32>) {
    let (width, pitch) = (shape.Width as usize, shape.Pitch as usize);
    let kind = shape.Type as i32;
    if kind == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 || kind == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 {
        let height = shape.Height as usize;
        let pixel = |x: usize, y: usize| {
            let [blue, green, red, alpha] = buffer[y * pitch + x * 4..][..4].try_into().unwrap();
            if kind == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 {
                // Alpha 0 replaces what is under it; 255 combines with it, which leaves black clear.
                let replaces = alpha == 0 || (blue, green, red) != (0, 0, 0);
                let [blue, green, red] = if alpha == 0 { [blue, green, red] } else { [0; 3] };
                return u32::from_le_bytes([blue, green, red, if replaces { 255 } else { 0 }]);
            }
            let times = |colour: u8| (colour as u16 * alpha as u16 / 255) as u8;
            u32::from_le_bytes([times(blue), times(green), times(red), alpha])
        };
        let pixels = (0..height).flat_map(|y| (0..width).map(move |x| (x, y))).map(|(x, y)| pixel(x, y)).collect();
        return (width, height, pixels);
    }
    // Black and white: a mask that keeps what is under the pointer, then one that inverts it, a bit a pixel.
    let height = shape.Height as usize / 2;
    let bit = |y: usize, x: usize| buffer[y * pitch + x / 8] >> (7 - x % 8) & 1 == 1;
    let pixels = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| match (bit(y, x), bit(y + height, x)) {
            (true, false) => 0,
            (false, true) => 0xFFFF_FFFF,
            _ => 0xFF00_0000,
        })
        .collect();
    (width, height, pixels)
}

/// Paints the pointer over the picture, rows of blue, green, red and a fourth byte.
fn paint(pointer: &Pointer, picture: &mut [u8], width: usize, height: usize) {
    let Some((left, top)) = pointer.at else { return };
    for (index, pixel) in pointer.pixels.iter().enumerate() {
        let (x, y) =
            (left as isize + (index % pointer.width) as isize, top as isize + (index / pointer.width) as isize);
        if x < 0 || y < 0 || x >= width as isize || y >= height as isize {
            continue;
        }
        let [blue, green, red, alpha] = pixel.to_le_bytes();
        let under = &mut picture[(y as usize * width + x as usize) * 4..][..3];
        for (under, over) in under.iter_mut().zip([blue, green, red]) {
            *under = over.saturating_add((*under as u16 * (255 - alpha as u16) / 255) as u8);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_MODE_ROTATION_IDENTITY;
    use windows::Win32::Graphics::Dxgi::DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME;

    #[test]
    fn padded_texture_rows_land_at_the_desktop_coordinates_for_each_rotation() {
        // A B C / D E F, with a fourth pixel of padding in each D3D row.
        let bytes: Vec<_> = b"ABC.DEF.".iter().flat_map(|&c| [c, 1, 2, 255]).collect();
        let mut picture = Picture::default();
        for (rotation, size, pixels) in [
            (DXGI_MODE_ROTATION_IDENTITY, (3, 2), b"ABCDEF"),
            (DXGI_MODE_ROTATION_ROTATE90, (2, 3), b"DAEBFC"),
            (DXGI_MODE_ROTATION_ROTATE180, (3, 2), b"FEDCBA"),
            (DXGI_MODE_ROTATION_ROTATE270, (2, 3), b"CFBEAD"),
        ] {
            picture.copy(&bytes, 16, 3, 2, rotation).unwrap();
            assert_eq!(picture.size, size);
            let expected: Vec<_> = pixels.iter().flat_map(|&c| [c, 1, 2, 255]).collect();
            assert_eq!(picture.bytes, expected);
        }
    }

    #[test]
    fn frame_deadlines_follow_the_requested_rate_and_restart_after_a_stall() {
        let start = Instant::now();
        let interval = Duration::from_secs(1) / 60;
        let second = next_frame(start, start, interval);
        assert_eq!(second, start + interval);
        let late = Duration::from_micros(800);
        assert_eq!(next_frame(second, second + late, interval), start + 2 * interval);
        let resumed = second + Duration::from_secs(1);
        assert_eq!(next_frame(second, resumed, interval), resumed + interval);
    }

    #[test]
    fn a_black_and_white_pointer_keeps_inverts_or_covers() {
        // Two pixels a row, one row: the first is kept, the second turned white.
        let shape = DXGI_OUTDUPL_POINTER_SHAPE_INFO {
            Type: DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32,
            Width: 2,
            Height: 2,
            Pitch: 1,
            ..Default::default()
        };
        let (width, height, pixels) = premultiplied(&shape, &[0b1000_0000, 0b0100_0000]);
        assert_eq!((width, height), (2, 1));
        assert_eq!(pixels, [0, 0xFFFF_FFFF]);
    }

    #[test]
    fn a_colour_pointer_is_multiplied_by_its_alpha_and_painted_over() {
        let shape = DXGI_OUTDUPL_POINTER_SHAPE_INFO {
            Type: DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 as u32,
            Width: 1,
            Height: 1,
            Pitch: 4,
            ..Default::default()
        };
        let (_, _, pixels) = premultiplied(&shape, &[200, 100, 50, 128]);
        assert_eq!(pixels, [u32::from_le_bytes([100, 50, 25, 128])]);
        let pointer = Pointer { width: 1, pixels, at: Some((1, 0)), ..Default::default() };
        let mut picture = [10u8; 8];
        paint(&pointer, &mut picture, 2, 1);
        assert_eq!(picture, [10, 10, 10, 10, 104, 54, 29, 10], "half of the pointer over half of what is under");
    }

    #[test]
    fn local_pointer_follows_visibility_even_without_a_new_shape() {
        let mut pointer = Pointer::default();
        let mut frame = DXGI_OUTDUPL_FRAME_INFO::default();
        assert_eq!(pointer.update_position(&frame), Some(false), "hide the local arrow on a baked-in first frame");
        assert_eq!(pointer.update_position(&frame), None);
        frame.LastMouseUpdateTime = 1;
        frame.PointerPosition.Visible = true.into();
        assert_eq!(pointer.update_position(&frame), Some(true));
        frame.PointerPosition.Position.x = 7;
        assert_eq!(pointer.update_position(&frame), None, "motion leaves the local cursor alone");
        assert_eq!(pointer.at, Some((7, 0)));
        assert_eq!(pointer.update_position(&Default::default()), None, "an image-only frame keeps visibility");
        frame.PointerPosition.Visible = false.into();
        assert_eq!(pointer.update_position(&frame), Some(false));
        frame.PointerPosition.Visible = true.into();
        assert_eq!(pointer.update_position(&frame), Some(true));
    }
}
