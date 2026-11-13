//! Colour conversion from the screen's BGRx pixels to planar YUV.
//!
//! H.264 that browsers decode in hardware is YUV, and the screen comes as RGB, so every changed frame goes
//! through here. The matrix and range (BT.709, full) are the ones the encoders announce in the stream.

use std::borrow::Cow;
use std::sync::Arc;

use pic_scale::{ImageSize, ImageStore, ImageStoreMut, Resampling, ResamplingFunction, Scaler};
use yuv::{BufferStoreMut, YuvConversionMode, YuvPlanarImageMut, YuvRange, YuvStandardMatrix};

use super::encode::Plane;

/// Fills the encoder's planes from `pixels` (rows of `stride` bytes, four bytes per pixel in B, G, R, x order).
pub fn bgrx_to_yuv(pixels: &[u8], stride: usize, width: usize, height: usize, planes: [Plane; 3], full_chroma: bool) {
    let [y, u, v] = planes;
    let mut image = YuvPlanarImageMut {
        y_plane: BufferStoreMut::Borrowed(y.bytes),
        y_stride: y.stride as u32,
        u_plane: BufferStoreMut::Borrowed(u.bytes),
        u_stride: u.stride as u32,
        v_plane: BufferStoreMut::Borrowed(v.bytes),
        v_stride: v.stride as u32,
        width: width as u32,
        height: height as u32,
    };
    let convert = if full_chroma { yuv::bgra_to_yuv444 } else { yuv::bgra_to_yuv420 };
    convert(&mut image, pixels, stride as u32, YuvRange::Full, YuvStandardMatrix::Bt709, YuvConversionMode::Balanced)
        .expect("the planes were allocated for this picture size");
}

/// A BGRx pixel's size: blue, green, red and a byte that is ignored.
pub const BYTES_PER_PIXEL: usize = 4;

/// Shrinks pictures of BGRx pixels from one size to a smaller one. The work is planned once for the two sizes;
/// pic-scale does it with the processor's vector instructions.
pub struct Shrinker {
    plan: Arc<Resampling<u8, BYTES_PER_PIXEL>>,
    scratch: Vec<u8>,
    from: (usize, usize),
    to: (usize, usize),
}

impl Shrinker {
    pub fn new(from: (usize, usize), to: (usize, usize)) -> Result<Self, String> {
        let size = |(width, height)| ImageSize::new(width, height);
        // Area, after OpenCV's INTER_AREA, averages what each pixel covers and a little around it: edges soften
        // a little, and fine patterns such as text turn to grey rather than to stray dots. The fourth byte is
        // ignored, and so is not treated as alpha.
        let plan = Scaler::new(ResamplingFunction::Area)
            .plan_rgba_resampling(size(from), size(to), false)
            .map_err(|error| format!("cannot shrink the picture: {error}"))?;
        Ok(Self { scratch: plan.alloc_scratch(), plan, from, to })
    }

    /// Shrinks `pixels`, in rows `stride` bytes apart, into `out`, which gets rows of `to.0 * BYTES_PER_PIXEL`
    /// bytes.
    pub fn shrink(&mut self, pixels: &[u8], stride: usize, out: &mut Vec<u8>) {
        let ((width, height), (to_width, to_height)) = (self.from, self.to);
        out.resize(to_width * to_height * BYTES_PER_PIXEL, 0);
        let source = ImageStore::<u8, BYTES_PER_PIXEL> {
            buffer: Cow::Borrowed(pixels),
            channels: BYTES_PER_PIXEL,
            width,
            height,
            stride,
            bit_depth: 8,
        };
        let shrunk = ImageStoreMut::borrow(out, to_width, to_height)
            .and_then(|mut into| self.plan.resample_with_scratch(&source, &mut into, &mut self.scratch));
        if let Err(error) = shrunk {
            warn!("could not shrink the picture: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the matrix and range: these are the BT.709 full-range values the stream's colour description promises.
    #[test]
    fn known_colours() {
        // B, G, R, x for white, black, red and blue, each filling one 2x2 block.
        let colours = [[255, 255, 255, 0], [0, 0, 0, 0], [0, 0, 255, 0], [255, 0, 0, 0]];
        let expected = [(255, 128, 128), (0, 128, 128), (54, 99, 255), (18, 255, 116)];
        for (colour, expected) in colours.into_iter().zip(expected) {
            let pixels = colour.repeat(4);
            for full_chroma in [false, true] {
                let chroma_size = if full_chroma { 4 } else { 1 };
                let (mut y, mut u, mut v) = (vec![0; 4], vec![0; chroma_size], vec![0; chroma_size]);
                let chroma_stride = if full_chroma { 2 } else { 1 };
                let planes = [
                    Plane { bytes: &mut y, stride: 2 },
                    Plane { bytes: &mut u, stride: chroma_stride },
                    Plane { bytes: &mut v, stride: chroma_stride },
                ];
                bgrx_to_yuv(&pixels, 8, 2, 2, planes, full_chroma);
                assert_eq!((y[0], u[0], v[0]), expected, "{colour:?} full_chroma={full_chroma}");
            }
        }
    }

    #[test]
    fn shrinking_keeps_even_areas_and_averages_fine_detail() {
        // 64x8, stored with a spare pixel at the end of each row: one-pixel stripes on the left, grey on the
        // right.
        let (width, height, stride) = (64, 8, 65 * BYTES_PER_PIXEL);
        let pixel = |x: usize| match x {
            0..32 => [if x.is_multiple_of(2) { 0 } else { 255 }; BYTES_PER_PIXEL],
            32..64 => [100; BYTES_PER_PIXEL],
            _ => [7; BYTES_PER_PIXEL],
        };
        let pixels: Vec<u8> = (0..height).flat_map(|_| (0..65).flat_map(pixel)).collect();
        let mut out = Vec::new();
        Shrinker::new((width, height), (32, 4)).unwrap().shrink(&pixels, stride, &mut out);
        assert_eq!(out.len(), 32 * 4 * BYTES_PER_PIXEL);
        let row: Vec<u8> = out[..32 * BYTES_PER_PIXEL].iter().step_by(BYTES_PER_PIXEL).copied().collect();
        assert!(row[4..12].iter().all(|&value| value.abs_diff(128) <= 1), "stripes turn grey: {row:?}");
        assert!(row[20..].iter().all(|&value| value == 100), "an even area stays as it is: {row:?}");
    }
}
