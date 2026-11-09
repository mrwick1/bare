//! Image decoders for `<img>`: PNG, JPEG and WebP into `0xAARRGGBB` pixels (not premultiplied).
//!
//! Each format has two steps. `size` reads the header only and allocates nothing. `decode` checks the same limits, then reads the
//! pixels. A file over a limit fails with an error before any pixel buffer exists: a bad image cannot use the memory of the page.
//! The crates are `png`, `zune-jpeg` and `image-webp` (pure Rust). GIF and animation are not supported: the first frame of an
//! animated WebP is decoded and the rest is not read.

use std::io::Cursor;
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
    pub pixels: Vec<u32>,
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

pub fn decode(b: &[u8]) -> Result<Image, Error> {
    let (width, height) = size(b)?;
    let n = width as usize * height as usize;
    let mut pixels = Vec::with_capacity(n);
    match kind(b).ok_or(Error::Unsupported)? {
        Kind::Png => {
            let mut d = png::Decoder::new(Cursor::new(b));
            d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
            let mut r = d.read_info().map_err(|_| Error::Corrupt)?;
            let mut buf = vec![0u8; r.output_buffer_size().ok_or(Error::Pixels)?];
            let f = r.next_frame(&mut buf).map_err(|_| Error::Corrupt)?;
            let data = &buf[..f.buffer_size()];
            match f.color_type {
                png::ColorType::Rgba => pixels.extend(data.chunks_exact(4).map(|p| argb(p[3], p[0], p[1], p[2]))),
                png::ColorType::Rgb => pixels.extend(data.chunks_exact(3).map(|p| argb(255, p[0], p[1], p[2]))),
                png::ColorType::GrayscaleAlpha => pixels.extend(data.chunks_exact(2).map(|p| argb(p[1], p[0], p[0], p[0]))),
                png::ColorType::Grayscale => pixels.extend(data.iter().map(|&g| argb(255, g, g, g))),
                png::ColorType::Indexed => return Err(Error::Corrupt),
            }
        }
        Kind::Jpeg => {
            let opts = zune_jpeg::zune_core::options::DecoderOptions::default().jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGBA);
            let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(b), opts);
            d.decode_headers().map_err(|_| Error::Corrupt)?;
            let mut buf = vec![0u8; d.output_buffer_size().ok_or(Error::Pixels)?];
            d.decode_into(&mut buf).map_err(|_| Error::Corrupt)?;
            if buf.len() < n * 4 {
                return Err(Error::Corrupt);
            }
            pixels.extend(buf.chunks_exact(4).take(n).map(|p| argb(255, p[0], p[1], p[2])));
        }
        Kind::Webp => {
            let mut d = image_webp::WebPDecoder::new(Cursor::new(b)).map_err(|_| Error::Corrupt)?;
            let mut buf = vec![0u8; d.output_buffer_size().ok_or(Error::Pixels)?];
            d.read_image(&mut buf).map_err(|_| Error::Corrupt)?;
            if d.has_alpha() {
                pixels.extend(buf.chunks_exact(4).map(|p| argb(p[3], p[0], p[1], p[2])));
            } else {
                pixels.extend(buf.chunks_exact(3).map(|p| argb(255, p[0], p[1], p[2])));
            }
        }
    }
    if pixels.len() != n {
        return Err(Error::Corrupt);
    }
    Ok(Image { width, height, pixels })
}

fn argb(a: u8, r: u8, g: u8, b: u8) -> u32 {
    u32::from(a) << 24 | u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)
}

#[cfg(test)]
mod tests;
