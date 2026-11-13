//! Screen capture on X11. The server copies the root window into shared memory when asked, and its damage
//! extension says when the screen has changed, so a still screen costs nothing here either.

use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::damage::{ConnectionExt as _, ReportLevel};
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xfixes::{ConnectionExt as _, CursorNotifyMask, GetCursorImageReply};
use x11rb::protocol::xproto::{ChangeWindowAttributesAux, ConnectionExt as _, EventMask, ImageFormat, ImageOrder};

use super::memory::Memory;
use crate::capture::Shape;
use crate::platform::linux::xserver::{self, Events};
use crate::screen;
use crate::stream::Stream;

/// Captures the chosen screen until the stream is no longer wanted.
pub fn run(mut stream: Stream) -> Result<(), String> {
    // A screen that changes size is simply captured anew.
    while capture(&mut stream).map_err(|error| format!("no Wayland compositor, and the X server: {error}"))?
        == Ended::Resized
    {}
    stream.ended.take().unwrap_or(Ok(()))
}

#[derive(PartialEq)]
enum Ended {
    Resized,
    Unwanted,
}

fn capture(stream: &mut Stream) -> Result<Ended, Box<dyn std::error::Error>> {
    let (x, screen) = x11rb::connect(None)?;
    let setup = x.setup();
    let screen = &setup.roots[screen];
    let (root, whole) = (screen.root, (screen.width_in_pixels, screen.height_in_pixels));
    // The server's screen spans all monitors. The chosen one is a part of it; where the server lists none, the
    // whole is captured.
    let monitors = xserver::screens().unwrap_or_default();
    let (left, top, width, height) = match screen::choose(&monitors, stream.settings.screen.as_deref()) {
        Some(monitor) => (monitor.x as i16, monitor.y as i16, monitor.width as u16, monitor.height as u16),
        None => (0, 0, whole.0, whole.1),
    };

    // The stream takes pixels as blue, green, red and a fourth byte, which is how nearly every X server
    // lays out its usual 24-bit visual.
    let visual = screen
        .allowed_depths
        .iter()
        .flat_map(|depth| &depth.visuals)
        .find(|visual| visual.visual_id == screen.root_visual);
    let bits = setup
        .pixmap_formats
        .iter()
        .find(|format| format.depth == screen.root_depth)
        .map(|format| format.bits_per_pixel);
    let usual = visual.is_some_and(|visual| (visual.red_mask, visual.blue_mask) == (0xFF0000, 0xFF));
    if !usual || bits != Some(32) || setup.image_byte_order != ImageOrder::LSB_FIRST {
        return Err("its pixel format is not one this program can capture".into());
    }

    let mut memory = Memory::new(width as usize * height as usize * 4)?;
    let segment = x.generate_id()?;
    x.shm_attach_fd(segment, memory.file.try_clone()?, false)?.check()?;
    x.damage_query_version(1, 1)?.reply()?;
    let damage = x.generate_id()?;
    x.damage_create(damage, root, ReportLevel::NON_EMPTY)?.check()?;
    x.xfixes_query_version(4, 0)?.reply()?;
    // Asks to be told when the root window, and so the screen, changes size.
    x.change_window_attributes(root, &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY))?
        .check()?;

    // Where the client draws the pointer, it is told the pointer's shape, now and whenever that changes.
    if !stream.settings.cursor {
        x.xfixes_select_cursor_input(root, CursorNotifyMask::DISPLAY_CURSOR)?.check()?;
        stream.pointer(shape(&x.xfixes_get_cursor_image()?.reply()?));
    }

    // The picture in the stream is out of date: at first because there is none.
    let mut stale = true;
    let mut captured = Instant::now() - Duration::from_secs(1);
    let mut pointer = None;
    let mut events = Events::default();
    while stream.wanted() {
        while let Some(event) = events.next(&x)? {
            match event {
                Event::DamageNotify(_) => stale = true,
                Event::XfixesCursorNotify(_) => stream.pointer(shape(&x.xfixes_get_cursor_image()?.reply()?)),
                Event::ConfigureNotify(change) if change.window == root && (change.width, change.height) != whole => {
                    return Ok(Ended::Resized);
                }
                _ => {}
            }
        }
        // The pointer is not part of what the server copies, and its movement is no damage: where the client
        // wants it in the picture, it is watched and painted here.
        if stream.settings.cursor {
            let now = x.xfixes_get_cursor_image()?.reply()?;
            let seen = (now.x, now.y, now.cursor_serial);
            stale |= pointer
                .as_ref()
                .is_none_or(|before: &GetCursorImageReply| (before.x, before.y, before.cursor_serial) != seen);
            pointer = Some(now);
        }
        let due = captured + stream.frame_interval();
        if stale && Instant::now() >= due {
            x.damage_subtract(damage, x11rb::NONE, x11rb::NONE)?;
            let copied =
                x.shm_get_image(root, left, top, width, height, !0, ImageFormat::Z_PIXMAP.into(), segment, 0)?.reply();
            // A screen that has just shrunk refuses the copy before its announcement of that has been read.
            if copied.is_err() {
                let now = x.get_geometry(root)?.reply()?;
                if (now.width, now.height) != whole {
                    return Ok(Ended::Resized);
                }
            }
            copied?;
            if let Some(pointer) = &pointer {
                paint(pointer, (left, top), memory.bytes(), width as usize, height as usize);
            }
            stream.picture(memory.bytes(), width as usize * 4, width as usize, height as usize);
            (stale, captured) = (false, Instant::now());
        }
        stream.refine();

        let mut wait = stream.patience();
        if stale {
            wait = wait.min(due.saturating_duration_since(Instant::now()));
        } else if stream.settings.cursor {
            wait = wait.min(stream.frame_interval());
        }
        events.wait(&x, None, wait)?;
    }
    Ok(Ended::Unwanted)
}

/// The pointer's shape as the client is told it.
fn shape(pointer: &GetCursorImageReply) -> Shape {
    let pixels = pointer.cursor_image.iter().copied();
    Shape::from_premultiplied(pointer.width, pointer.height, (pointer.xhot, pointer.yhot), pixels)
}

/// Paints the pointer over the picture, whose top left corner is at `origin` on the server's screen. The
/// pointer's image is rows of pixels with alpha, the colours already multiplied by it; the picture is rows of
/// blue, green, red and a fourth byte.
fn paint(pointer: &GetCursorImageReply, origin: (i16, i16), picture: &mut [u8], width: usize, height: usize) {
    let corner = |at: i16, hot: u16, origin: i16| at as isize - hot as isize - origin as isize;
    let (left, top) = (corner(pointer.x, pointer.xhot, origin.0), corner(pointer.y, pointer.yhot, origin.1));
    for (index, pixel) in pointer.cursor_image.iter().enumerate() {
        let (x, y) =
            (left + (index % pointer.width as usize) as isize, top + (index / pointer.width as usize) as isize);
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

    #[test]
    fn the_pointer_is_blended_over_the_picture_and_clipped_at_its_edges() {
        // A 2x2 pointer: opaque red, half-transparent white (premultiplied), fully transparent, opaque blue.
        let cursor_image = vec![0xFFFF_0000, 0x8080_8080, 0x0000_0000, 0xFF00_00FF];
        let pointer = |x, y| GetCursorImageReply {
            x,
            y,
            width: 2,
            height: 2,
            xhot: 0,
            yhot: 0,
            cursor_image: cursor_image.clone(),
            ..Default::default()
        };
        let mut picture = vec![100; 3 * 2 * 4]; // 3x2 pixels of grey
        paint(&pointer(1, 0), (0, 0), &mut picture, 3, 2);
        let pixel = |picture: &[u8], x: usize, y: usize| picture[(y * 3 + x) * 4..][..3].to_vec();
        assert_eq!(pixel(&picture, 0, 0), [100, 100, 100], "untouched beside the pointer");
        assert_eq!(pixel(&picture, 1, 0), [0, 0, 255], "opaque red replaces the picture");
        assert_eq!(pixel(&picture, 2, 0), [177, 177, 177], "half-transparent white lightens it");
        assert_eq!(pixel(&picture, 1, 1), [100, 100, 100], "a transparent pixel leaves it");
        assert_eq!(pixel(&picture, 2, 1), [255, 0, 0], "opaque blue replaces it");
        // Hanging over the right and bottom edges, only the pointer's first pixel lands in the picture. So it
        // does in a picture of a monitor that begins further right and down on the server's screen.
        let mut picture = vec![100; 3 * 2 * 4];
        paint(&pointer(12, 21), (10, 20), &mut picture, 3, 2);
        assert_eq!(pixel(&picture, 2, 1), [0, 0, 255]);
        assert_eq!(picture.iter().filter(|&&byte| byte != 100).count(), 3);
    }
}
