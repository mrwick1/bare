//! JPEG for `toDataURL` and `toBlob` of a canvas: the `jpeg-encoder` crate. Its DCT is based in part on the work of the Independent
//! JPEG Group (`LICENSE-IJG`).

use jpeg_encoder::{Encoder, ImageBuffer, JpegColorType, rgb_to_ycbcr};

/// Premultiplied `0xAARRGGBB` pixels read as RGB: the color on black, as Chrome encodes a canvas. `None`: a blank canvas (black).
struct OnBlack<'a>(Option<&'a [u32]>, u16, u16);

impl ImageBuffer for OnBlack<'_> {
    fn get_jpeg_color_type(&self) -> JpegColorType {
        JpegColorType::Ycbcr
    }
    fn width(&self) -> u16 {
        self.1
    }
    fn height(&self) -> u16 {
        self.2
    }
    fn fill_buffers(&self, y: u16, buffers: &mut [Vec<u8>; 4]) {
        let w = self.1 as usize;
        for x in 0..w {
            let p = self.0.map_or(0, |px| px[y as usize * w + x]);
            let (l, cb, cr) = rgb_to_ycbcr((p >> 16) as u8, (p >> 8) as u8, p as u8);
            buffers[0].push(l);
            buffers[1].push(cb);
            buffers[2].push(cr);
        }
    }
}

/// A JPEG of `w` by `h` premultiplied pixels at `quality` (1 to 100). `None`: a side over 65,535 (JPEG cannot hold it).
pub fn encode(w: u32, h: u32, premul: Option<&[u32]>, quality: u8) -> Option<Vec<u8>> {
    let (w, h) = (u16::try_from(w).ok()?, u16::try_from(h).ok()?);
    let mut out = Vec::new();
    Encoder::new(&mut out, quality).encode_image(OnBlack(premul, w, h)).ok()?;
    Some(out)
}
