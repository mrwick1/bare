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
