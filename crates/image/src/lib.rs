//! Image decoders for `<img>`: PNG, JPEG and WebP into `0xAARRGGBB` pixels (not premultiplied).
//!
//! Each format has two steps. `size` reads the header only and allocates nothing. `decode` checks the same limits, then reads the
//! pixels. A file over a limit fails with an error before any pixel buffer exists: a bad image cannot use the memory of the page.
//! The crates are `png`, `zune-jpeg` and `image-webp` (pure Rust). GIF and animation are not supported: the first frame of an
//! animated WebP is decoded and the rest is not read.

use std::io::Cursor;
use std::sync::Arc;
use zune_jpeg::zune_core::bytestream::ZCursor;

/// The bytes of one file. The caller reads at most this much from the network.
pub const MAX_BYTES: usize = 32 << 20;
/// The pixels of one image. 16 M pixels are 64 MiB of decoded memory: a larger picture is refused, not scaled.
pub const MAX_PIXELS: u64 = 16 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
    Webp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// More than `MAX_BYTES` of file.
    TooLarge,
    /// A side of 0, or more than `MAX_PIXELS` pixels.
    Pixels,
    /// Not PNG, JPEG or WebP.
    Unsupported,
    /// The header or the data is damaged.
    Corrupt,
}

pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<[u32]>,
}

/// The format of a file, from its first bytes.
pub fn kind(b: &[u8]) -> Option<Kind> {
    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some(Kind::Png)
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(Kind::Jpeg)
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some(Kind::Webp)
    } else {
        None
    }
}

fn check(w: u32, h: u32) -> Result<(u32, u32), Error> {
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS { Err(Error::Pixels) } else { Ok((w, h)) }
}

/// The size of the picture, from the header only.
pub fn size(b: &[u8]) -> Result<(u32, u32), Error> {
    if b.len() > MAX_BYTES {
        return Err(Error::TooLarge);
    }
    match kind(b).ok_or(Error::Unsupported)? {
        Kind::Png => {
            let r = png::Decoder::new(Cursor::new(b)).read_info().map_err(|_| Error::Corrupt)?;
            let i = r.info();
            check(i.width, i.height)
        }
        Kind::Jpeg => {
            let mut d = zune_jpeg::JpegDecoder::new(ZCursor::new(b));
            d.decode_headers().map_err(|_| Error::Corrupt)?;
            let i = d.info().ok_or(Error::Corrupt)?;
            check(u32::from(i.width), u32::from(i.height))
        }
        Kind::Webp => {
            let d = image_webp::WebPDecoder::new(Cursor::new(b)).map_err(|_| Error::Corrupt)?;
            let (w, h) = d.dimensions();
            check(w, h)
        }
    }
}

/// Decode a file. The pixels are made in their final buffer: the decoder writes its output (3 or 4 bytes for each pixel) into the
/// start of it, and the conversion to `0xAARRGGBB` runs in place, from the last pixel to the first so that a 3 byte pixel is read
/// before a 4 byte one covers it. A file never holds a staging buffer, a `Vec` of pixels or a copy for the `Arc`: the peak is the
/// pixels, 4 bytes each, and what the decoder keeps for itself.
pub fn decode(b: &[u8]) -> Result<Image, Error> {
    let (width, height) = size(b)?;
    let n = width as usize * height as usize;
    let pixels = Arc::<[u32]>::new_zeroed_slice(n);
    // SAFETY: all bits zero is a valid `u32`.
    let mut pixels = unsafe { pixels.assume_init() };
    let words = Arc::get_mut(&mut pixels).ok_or(Error::Corrupt)?;
    // SAFETY: the same memory as `words`, which is not used while `bytes` lives. `u8` has alignment 1 and takes any bit pattern.
    let bytes = unsafe { std::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), n * 4) };
    // The bytes that the decoder gave for each pixel.
    let channels = match kind(b).ok_or(Error::Unsupported)? {
        Kind::Png => {
            let mut d = png::Decoder::new(Cursor::new(b));
            d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
            let mut r = d.read_info().map_err(|_| Error::Corrupt)?;
            let size = r.output_buffer_size().ok_or(Error::Pixels)?;
            let out = bytes.get_mut(..size).ok_or(Error::Pixels)?;
            let f = r.next_frame(out).map_err(|_| Error::Corrupt)?;
            let channels = match f.color_type {
                png::ColorType::Rgba => 4,
                png::ColorType::Rgb => 3,
                png::ColorType::GrayscaleAlpha => 2,
                png::ColorType::Grayscale => 1,
                png::ColorType::Indexed => return Err(Error::Corrupt),
            };
            if f.buffer_size() != n * channels {
                return Err(Error::Corrupt);
            }
            widen(bytes, n, channels, |p| match *p {
                [r, g, b, a] => argb(a, r, g, b),
                [r, g, b] => argb(255, r, g, b),
                [g, a] => argb(a, g, g, g),
                _ => argb(255, p[0], p[0], p[0]),
            });
            return Ok(Image { width, height, pixels });
        }
        Kind::Jpeg => {
            let opts = zune_jpeg::zune_core::options::DecoderOptions::default().jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGBA);
            let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(b), opts);
            d.decode_headers().map_err(|_| Error::Corrupt)?;
            if d.output_buffer_size() != Some(n * 4) {
                return Err(Error::Corrupt);
            }
            d.decode_into(bytes).map_err(|_| Error::Corrupt)?;
            widen(bytes, n, 4, |p| argb(255, p[0], p[1], p[2]));
            return Ok(Image { width, height, pixels });
        }
        Kind::Webp => {
            let mut d = image_webp::WebPDecoder::new(Cursor::new(b)).map_err(|_| Error::Corrupt)?;
            let size = d.output_buffer_size().ok_or(Error::Pixels)?;
            let channels = if d.has_alpha() { 4 } else { 3 };
            if size != n * channels {
                return Err(Error::Corrupt);
            }
            d.read_image(&mut bytes[..size]).map_err(|_| Error::Corrupt)?;
            channels
        }
    };
    widen(bytes, n, channels, |p| if channels == 4 { argb(p[3], p[0], p[1], p[2]) } else { argb(255, p[0], p[1], p[2]) });
    Ok(Image { width, height, pixels })
}

/// Turn `n` pixels of `channels` bytes each, at the start of `bytes`, into `n` words of 4 bytes (`u32` in native order), in place.
fn widen(bytes: &mut [u8], n: usize, channels: usize, to_word: impl Fn(&[u8]) -> u32) {
    for i in (0..n).rev() {
        let word = to_word(&bytes[i * channels..(i + 1) * channels]);
        bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_ne_bytes());
    }
}

fn argb(a: u8, r: u8, g: u8, b: u8) -> u32 {
    u32::from(a) << 24 | u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)
}

#[cfg(test)]
mod tests;
