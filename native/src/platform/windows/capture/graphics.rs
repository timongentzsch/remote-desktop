//! Windows Graphics Capture can exclude a software pointer that Desktop Duplication puts in the pixels.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::{Common::DXGI_MODE_ROTATION_IDENTITY, IDXGIDevice};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
use windows::core::{Interface, factory};

use super::{CHECK_DISPLAYS, Picture, cursor::LocalCursor, next_frame, same_display, take};
use crate::screen::Screen;

/// How long Windows may take over the first picture before this way of capturing counts as failed, and the
/// longest the loop waits for the next one before it looks at the stream and the displays again.
const FIRST_PICTURE: Duration = Duration::from_secs(2);
const LONGEST_WAIT: Duration = Duration::from_millis(50);
use crate::stream::Stream;

struct Apartment;

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}

struct Capture {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

pub(super) fn run(
    stream: &mut Stream,
    screen: &Screen,
    monitor: HMONITOR,
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
) -> Result<(), Box<dyn std::error::Error>> {
    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }?;
    let _apartment = Apartment;
    let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(monitor) }?;
    let size = item.Size()?;
    let dxgi: IDXGIDevice = device.cast()?;
    let winrt: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }?.cast()?;
    let pool =
        Direct3D11CaptureFramePool::CreateFreeThreaded(&winrt, DirectXPixelFormat::B8G8R8A8UIntNormalized, 3, size)?;
    let session = pool.CreateCaptureSession(&item)?;
    let capture = Capture { pool, session };
    capture.session.SetIsCursorCaptureEnabled(false)?;
    // No frame around the display at the machine itself, where Windows lets a program say so (11).
    let _ = capture.session.SetIsBorderRequired(false);
    // WGC's 16ms minimum can skip every other refresh on a 64Hz display. Give the source
    // headroom; the worker below sets the output rate. Older Windows lacks this property.
    let _ = capture.session.SetMinUpdateInterval((stream.frame_interval() / 2).into());

    // Each notification replaces any unread frame. Encoding a slow frame never builds a capture backlog.
    let latest = Arc::new(Mutex::new(None));
    let worker = thread::current();
    // Releasing the mailbox returns its last frame before the session and pool are closed.
    let pending = Arc::downgrade(&latest);
    capture.pool.FrameArrived(&TypedEventHandler::new(
        move |pool: windows::core::Ref<Direct3D11CaptureFramePool>, _| {
            let Some(pending) = pending.upgrade() else { return Ok(()) };
            let frame = pool.ok()?.TryGetNextFrame();
            if let Some(Ok(skipped)) = pending.lock().unwrap().replace(frame) {
                let _ = skipped.Close();
            }
            worker.unpark();
            Ok(())
        },
    ))?;
    let closed = Arc::new(AtomicBool::new(false));
    let ended = Arc::clone(&closed);
    let worker = thread::current();
    item.Closed(&TypedEventHandler::new(move |_, _| {
        ended.store(true, Ordering::Relaxed);
        worker.unpark();
        Ok(())
    }))?;
    capture.session.StartCapture()?;

    let started = Instant::now();
    let mut due = started;
    let mut check_displays = started + CHECK_DISPLAYS;
    let mut picture = Picture::default();
    let mut staging = None;
    let mut cursor = LocalCursor::default();
    while stream.wanted() && !closed.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now >= check_displays {
            if !same_display(stream, screen)? {
                break;
            }
            check_displays = now + CHECK_DISPLAYS;
        }
        let ready = if now >= due { latest.lock().unwrap().take() } else { None };
        if let Some(frame) = ready {
            let frame = frame?;
            if frame.ContentSize()? != size {
                break;
            }
            let surface: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
            let texture: ID3D11Texture2D = unsafe { surface.GetInterface() }?;
            take(context, device, &texture, &mut staging, DXGI_MODE_ROTATION_IDENTITY, &mut picture)?;
            frame.Close()?;
            due = next_frame(due, now, stream.frame_interval());
            let (width, height) = picture.size;
            cursor.update(stream, monitor);
            stream.picture(&picture.bytes, width * 4, width, height);
        } else if picture.bytes.is_empty() && started.elapsed() >= FIRST_PICTURE {
            return Err("no first frame from Windows Graphics Capture".into());
        } else if !picture.bytes.is_empty() {
            cursor.update(stream, monitor);
        }
        if stream.refine() && !picture.bytes.is_empty() {
            let (width, height) = picture.size;
            stream.picture(&picture.bytes, width * 4, width, height);
        }
        let mut wait = stream.patience().min(LONGEST_WAIT);
        if latest.lock().unwrap().is_some() {
            wait = wait.min(due.saturating_duration_since(Instant::now()));
        }
        thread::park_timeout(wait);
    }
    Ok(())
}
