//! The screen's picture through Desktop Duplication: Windows hands over each new frame of the chosen display, as
//! blue, green, red and alpha, and none while the screen is still. The pointer comes apart from the picture, as
//! its shape and where it is; it is painted in here where the client wants it in the picture, and its shape is
//! passed on where the client draws its own. A display that changes mode is captured anew.

use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_FLAG, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_POINTER_SHAPE_INFO,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

use super::outputs;
use crate::capture::{Settings, Shape, Shared, Sink};
use crate::stream::Stream;

/// How long the display may refuse to be captured: while it changes mode, or the secure desktop of a UAC
/// prompt is shown.
const REFUSING: Duration = Duration::from_secs(8);

/// Captures the chosen screen until the stream is no longer wanted.
pub fn run(settings: Settings, shared: Arc<Shared>, sink: Sink) -> Result<(), String> {
    let mut stream = Stream::new(settings, shared, sink);
    let mut refused = None;
    while stream.wanted() {
        match capture(&mut stream) {
            Ok(()) => refused = None,
            Err(error) if refused.get_or_insert_with(Instant::now).elapsed() > REFUSING => return Err(error),
            Err(_) => std::thread::sleep(stream.frame_interval()),
        }
    }
    stream.ended.take().unwrap_or(Ok(()))
}

/// The pointer as Desktop Duplication tells it: its shape, as pixels with alpha whose colours are multiplied by
/// it, and where its top left corner is on the display, if it is shown there.
#[derive(Default)]
struct Pointer {
    width: usize,
    pixels: Vec<u32>,
    at: Option<(i32, i32)>,
}

/// Says what of the capture failed, and how.
fn failed(what: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |error| format!("the screen cannot be captured: {what}: {error}")
}

/// Captures the chosen display until another is chosen, its mode changes, or the stream is no longer wanted.
fn capture(stream: &mut Stream) -> Result<(), String> {
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
            D3D11_CREATE_DEVICE_FLAG(0),
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(failed("no Direct3D device"))?;
    let (device, context) = (device.unwrap(), context.unwrap());
    let duplication = unsafe { output.cast::<IDXGIOutput1>().and_then(|output| output.DuplicateOutput(&device)) }
        .map_err(failed("Desktop Duplication refused"))?;
    let (width, height) = (screen.width as usize, screen.height as usize);

    // The latest picture as it came, and the copy handed on, with the pointer painted in where it is wanted.
    let mut picture = Vec::new();
    let mut shown = Vec::new();
    let mut staging = None;
    let mut pointer = Pointer::default();
    let mut stale = false;
    let mut captured = Instant::now() - Duration::from_secs(1);
    let unchanged = |stream: &Stream| {
        let outputs = super::screens().unwrap_or_default();
        crate::screen::choose(&outputs, stream.settings.screen.as_deref()) == Some(&screen)
    };
    while stream.wanted() && unchanged(stream) {
        let due = captured + stream.frame_interval();
        let mut wait = stream.patience();
        if stale {
            wait = wait.min(due.saturating_duration_since(Instant::now()));
        }
        let (mut info, mut resource) = (DXGI_OUTDUPL_FRAME_INFO::default(), None::<IDXGIResource>);
        match unsafe { duplication.AcquireNextFrame(wait.as_millis() as u32, &mut info, &mut resource) } {
            Ok(()) => {
                let taken = take(&context, &device, &info, resource, &mut staging, &mut picture);
                let shape = new_shape(&duplication, &info, &mut pointer);
                if info.LastMouseUpdateTime != 0 {
                    let position = info.PointerPosition;
                    pointer.at = position.Visible.as_bool().then_some((position.Position.x, position.Position.y));
                }
                unsafe { duplication.ReleaseFrame() }.map_err(failed("the frame"))?;
                taken.map_err(failed("the frame"))?;
                match shape {
                    Some(shape) if !stream.settings.cursor => stream.pointer(shape),
                    _ => {}
                }
                stale |= info.LastPresentTime != 0 || (stream.settings.cursor && info.LastMouseUpdateTime != 0);
            }
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => {}
            Err(error) if error.code() == DXGI_ERROR_ACCESS_LOST => return Ok(()),
            Err(error) => return Err(failed("the next frame")(error)),
        }
        if stale && !picture.is_empty() && Instant::now() >= due {
            let bytes = if stream.settings.cursor {
                shown.clone_from(&picture);
                paint(&pointer, &mut shown, width, height);
                &shown
            } else {
                &picture
            };
            stream.picture(bytes, width * 4, width, height);
            (stale, captured) = (false, Instant::now());
        }
        stream.refine();
    }
    Ok(())
}

/// Copies a new picture, where the frame has one, into `picture`, through a texture the processor can read.
fn take(
    context: &ID3D11DeviceContext,
    device: &ID3D11Device,
    info: &DXGI_OUTDUPL_FRAME_INFO,
    resource: Option<IDXGIResource>,
    staging: &mut Option<ID3D11Texture2D>,
    picture: &mut Vec<u8>,
) -> windows::core::Result<()> {
    let Some(texture) = resource.filter(|_| info.LastPresentTime != 0) else { return Ok(()) };
    let texture: ID3D11Texture2D = texture.cast()?;
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
    unsafe { context.CopyResource(staging, &texture) };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe { context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }?;
    let (width, height) = (description.Width as usize, description.Height as usize);
    picture.resize(width * height * 4, 0);
    for row in 0..height {
        let from = unsafe {
            std::slice::from_raw_parts(mapped.pData.cast::<u8>().add(row * mapped.RowPitch as usize), width * 4)
        };
        picture[row * width * 4..][..width * 4].copy_from_slice(from);
    }
    unsafe { context.Unmap(staging, 0) };
    Ok(())
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
    *pointer = Pointer { width, pixels, at: pointer.at };
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
    use windows::Win32::Graphics::Dxgi::DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME;

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
        let pointer = Pointer { width: 1, pixels, at: Some((1, 0)) };
        let mut picture = [10u8; 8];
        paint(&pointer, &mut picture, 2, 1);
        assert_eq!(picture, [10, 10, 10, 10, 104, 54, 29, 10], "half of the pointer over half of what is under");
    }
}
