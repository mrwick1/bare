//! Pixel capture: write a `Canvas` as a PNG.
//!
//! The `png` crate does the encoding. Measured on a 1280 by 800 page-like picture (`docs/resources.md`): 1.5 ms and
//! 261 KB with `Compression::Fast` and the `Sub` filter, against 4.1 MB for a writer with no compression. The
//! picture is RGB with no alpha channel, because a canvas is opaque. Rows go to the encoder one at a time, so the capture needs
//! no copy of the whole picture. The same canvas always gives the same bytes.

use crate::canvas::Canvas;
use std::io::{self, Write};

pub fn encode(canvas: &Canvas, out: impl Write) -> io::Result<()> {
    let mut e = png::Encoder::new(out, canvas.width, canvas.height);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.set_compression(png::Compression::Fast);
    e.set_filter(png::Filter::Sub);
    let mut writer = e.write_header().map_err(io::Error::other)?;
    let mut stream = writer.stream_writer().map_err(io::Error::other)?;
    let mut row = Vec::with_capacity(canvas.width as usize * 3);
    for line in canvas.pixels.chunks(canvas.width.max(1) as usize) {
        row.clear();
        for &p in line {
            row.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
        stream.write_all(&row)?;
    }
    stream.finish().map_err(io::Error::other)?;
    Ok(())
}

pub fn write_file(canvas: &Canvas, path: &std::path::Path) -> io::Result<()> {
    encode(canvas, io::BufWriter::new(std::fs::File::create(path)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut c = Canvas::new(3, 2);
        c.pixels[1] = 0xff10_2030;
        c.pixels[5] = 0xff00_ff00;
        let mut bytes = Vec::new();
        encode(&c, &mut bytes).unwrap();
        let mut reader = png::Decoder::new(io::Cursor::new(bytes)).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (3, 2));
        assert_eq!(&buf[..9], &[255, 255, 255, 0x10, 0x20, 0x30, 255, 255, 255]);
        assert_eq!(&buf[15..18], &[0, 255, 0]);
    }
}
