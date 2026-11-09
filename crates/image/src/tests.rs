use super::*;

const PNG: &[u8] = include_bytes!("../../../tools/fixtures/img/red.png");
const HALF: &[u8] = include_bytes!("../../../tools/fixtures/img/half.png");
const JPG: &[u8] = include_bytes!("../../../tools/fixtures/img/blue.jpg");
const WEBP: &[u8] = include_bytes!("../../../tools/fixtures/img/green.webp");

fn near(p: u32, want: (u8, u8, u8)) -> bool {
    let c = |s: u32| (p >> s) as u8;
    let d = |a: u8, b: u8| a.abs_diff(b) < 8;
    c(24) == 255 && d(c(16), want.0) && d(c(8), want.1) && d(c(0), want.2)
}

#[test]
fn the_header_gives_the_size_of_each_format() {
    assert_eq!(size(PNG), Ok((20, 10)));
    assert_eq!(size(JPG), Ok((20, 10)));
    assert_eq!(size(WEBP), Ok((20, 10)));
}

#[test]
fn the_header_is_enough_for_the_size() {
    // The size does not need the end of the file: the pixel data can be cut.
    let cut = &PNG[..PNG.len() - 20];
    assert_eq!(size(cut), Ok((20, 10)));
    assert_eq!(decode(cut).err(), Some(Error::Corrupt));
}

#[test]
fn each_format_decodes_to_its_colors() {
    for (file, want) in [(PNG, (255, 0, 0)), (JPG, (0, 0, 255)), (WEBP, (0, 255, 0))] {
        let i = decode(file).unwrap();
        assert_eq!((i.width, i.height, i.pixels.len()), (20, 10, 200));
        assert!(near(i.pixels[0], want) && near(i.pixels[199], want), "{:08x}", i.pixels[0]);
    }
}

#[test]
fn alpha_is_kept() {
    let i = decode(HALF).unwrap();
    assert_eq!(i.pixels[0], 0x80ff_ffff);
    assert_eq!(i.pixels[7], 0);
}

#[test]
fn an_unknown_file_is_refused() {
    assert_eq!(size(b"<svg></svg>").err(), Some(Error::Unsupported));
    assert_eq!(size(b"GIF89a\x01\0\x01\0").err(), Some(Error::Unsupported));
    assert_eq!(size(&[]).err(), Some(Error::Unsupported));
}

#[test]
fn a_big_picture_is_refused_from_the_header() {
    // A PNG header that says 65536 by 65536 (4 G pixels), with no data after it.
    let mut b = PNG[..33].to_vec();
    b[16..20].copy_from_slice(&65536u32.to_be_bytes());
    b[20..24].copy_from_slice(&65536u32.to_be_bytes());
    // The CRC of IHDR is now wrong: `png` may refuse it first. Either way the picture is not made.
    assert!(matches!(size(&b), Err(Error::Pixels | Error::Corrupt)));
    assert!(decode(&b).is_err());
    // A side of 0.
    let mut z = PNG[..33].to_vec();
    z[16..20].copy_from_slice(&0u32.to_be_bytes());
    assert!(size(&z).is_err());
}

#[test]
fn the_pixel_limit_is_checked_before_a_buffer_is_made() {
    assert_eq!(check(4096, 4096), Ok((4096, 4096)));
    assert_eq!(check(4097, 4096), Err(Error::Pixels));
    assert_eq!(check(u32::MAX, u32::MAX), Err(Error::Pixels));
    assert_eq!(check(0, 5), Err(Error::Pixels));
}

#[test]
fn a_file_over_the_byte_limit_is_refused() {
    let mut b = PNG.to_vec();
    b.resize(MAX_BYTES + 1, 0);
    assert_eq!(size(&b), Err(Error::TooLarge));
    assert_eq!(decode(&b).err(), Some(Error::TooLarge));
}

#[test]
fn damaged_data_fails_and_does_not_panic() {
    for file in [PNG, JPG, WEBP] {
        for cut in [file.len() / 2, file.len() - 1] {
            let _ = decode(&file[..cut]);
        }
        let mut b = file.to_vec();
        let n = b.len();
        b[n / 2] ^= 0xff;
        let _ = decode(&b);
    }
}

mod peak {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    /// Counts the live bytes and the highest count of the thread that allocates, so tests that run at the same time do not mix.
    struct Counting;
    thread_local! {
        static LIVE: Cell<usize> = const { Cell::new(0) };
        static PEAK: Cell<usize> = const { Cell::new(0) };
    }
    fn add(n: usize) {
        let live = LIVE.get() + n;
        LIVE.set(live);
        PEAK.set(PEAK.get().max(live));
    }
    // SAFETY: every call goes to `System` with the same arguments.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            add(l.size());
            unsafe { System.alloc(l) }
        }
        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            add(l.size());
            unsafe { System.alloc_zeroed(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            LIVE.set(LIVE.get().saturating_sub(l.size()));
            unsafe { System.dealloc(p, l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            LIVE.set(LIVE.get().saturating_sub(l.size()));
            add(n);
            unsafe { System.realloc(p, l, n) }
        }
    }
    #[global_allocator]
    static COUNT: Counting = Counting;

    fn png(w: u32, h: u32, color: png::ColorType) -> Vec<u8> {
        let mut out = Vec::new();
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(color);
        e.set_depth(png::BitDepth::Eight);
        let channels = color.samples();
        let mut e = e.write_header().unwrap();
        let row: Vec<u8> = (0..w as usize * channels).map(|i| (i * 7 % 251) as u8).collect();
        let mut data = Vec::new();
        for _ in 0..h {
            data.extend_from_slice(&row);
        }
        e.write_image_data(&data).unwrap();
        e.finish().unwrap();
        out
    }

    /// The bytes held at once while one picture decodes, above what was held before: at most the pixels (4 bytes each) and a small
    /// working set of the decoder. A staging buffer or a copy of the pixels would add another 4 bytes for each pixel.
    #[test]
    fn a_decode_holds_the_pixels_once() {
        for color in [png::ColorType::Rgba, png::ColorType::Rgb] {
            let file = png(1500, 1000, color);
            let before = LIVE.get();
            PEAK.set(before);
            let i = decode(&file).unwrap();
            let used = PEAK.get() - before;
            let pixels = 1500 * 1000 * 4;
            assert_eq!((i.width, i.height, i.pixels.len()), (1500, 1000, 1500 * 1000));
            assert!(used < pixels + pixels / 4, "{color:?}: {used} bytes held at the peak for {pixels} bytes of pixels");
        }
    }

    #[test]
    fn the_in_place_conversion_keeps_every_channel() {
        for (color, a) in [(png::ColorType::Rgba, None), (png::ColorType::Rgb, Some(255u8))] {
            let file = png(37, 11, color);
            let i = decode(&file).unwrap();
            let n = color.samples();
            for y in 0..11usize {
                for x in 0..37usize {
                    let s = |c: usize| ((x * n + c) * 7 % 251) as u8;
                    let want = argb(a.unwrap_or(s(3.min(n - 1))), s(0), s(1), s(2));
                    assert_eq!(i.pixels[y * 37 + x], want, "{color:?} at {x},{y}");
                }
            }
        }
        for (color, f) in [(png::ColorType::Grayscale, false), (png::ColorType::GrayscaleAlpha, true)] {
            let file = png(29, 5, color);
            let i = decode(&file).unwrap();
            let n = color.samples();
            for x in 0..29usize {
                let g = ((x * n) * 7 % 251) as u8;
                let a = if f { ((x * n + 1) * 7 % 251) as u8 } else { 255 };
                assert_eq!(i.pixels[x], argb(a, g, g, g), "{color:?} at {x}");
            }
        }
    }
}
