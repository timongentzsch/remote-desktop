//! Colour conversion from the compositor's BGRx pixels to planar YUV.
//!
//! H.264 that browsers decode in hardware is YUV, and the compositor only hands out RGB, so every changed frame
//! goes through here. The matrix and range (BT.709, full) are the ones the encoders announce in the stream.

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

/// Averages each 2x2 block of BGRx pixels into one, halving both dimensions. `out` gets rows of `width / 2 * 4` bytes.
pub fn halve(pixels: &[u8], stride: usize, width: usize, height: usize, out: &mut Vec<u8>) {
    out.clear();
    for pair in pixels.chunks_exact(stride * 2).take(height / 2) {
        let (top, bottom) = pair.split_at(stride);
        for (top, bottom) in top[..width / 2 * 8].chunks_exact(8).zip(bottom.chunks_exact(8)) {
            for channel in 0..4 {
                let sum =
                    top[channel] as u16 + top[channel + 4] as u16 + bottom[channel] as u16 + bottom[channel + 4] as u16;
                out.push(((sum + 2) / 4) as u8);
            }
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
    fn halving_averages_blocks() {
        // A 4x2 image: the left block is two black and two white pixels, the right block is solid.
        let row = |a: u8, b: u8| [[a; 4], [b; 4], [100; 4], [100; 4]].concat();
        let pixels = [row(0, 255), row(255, 0)].concat();
        let mut out = Vec::new();
        halve(&pixels, 16, 4, 2, &mut out);
        assert_eq!(out, [128, 128, 128, 128, 100, 100, 100, 100]);
    }
}
