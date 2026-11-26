//! Tests of the canvas raster (`raster2d.rs`). The expected values are Chrome's for the same draws, within the stated tolerance.

use crate::raster2d::*;
use std::sync::Arc;

fn rect(x: f32, y: f32, w: f32, h: f32) -> Vec<(f32, f32)> {
    vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
}

fn solid(path: Vec<(f32, f32)>, c: u32) -> Draw {
    Draw::Fill { path, evenodd: false, src: Source::Solid(premul(c)), alpha: 1.0, mode: Mode::SourceOver }
}

#[test]
fn a_record_rasters_only_at_a_read() {
    let mut r = Record::new(8, 8);
    for i in 0..10 {
        r.push(solid(rect(i as f32, 0.0, 1.0, 1.0), 0xFF00_00FF));
    }
    assert_eq!(r.rasters, 0, "no raster before a read");
    assert_eq!(r.rgba(0, 0, 1, 1), [0, 0, 255, 255]);
    assert_eq!(r.rasters, 1);
    r.rgba(0, 0, 1, 1);
    assert_eq!(r.rasters, 1, "a second read with no new draw plays nothing");
}

#[test]
fn a_fill_of_the_whole_canvas_drops_the_ops_before_it() {
    let mut r = Record::new(4, 4);
    r.push(solid(rect(1.0, 1.0, 1.0, 1.0), 0xFFFF_0000));
    r.push(Draw::Fill { path: rect(0.0, 0.0, 4.0, 4.0), evenodd: false, src: Source::Solid(0), alpha: 1.0, mode: Mode::Clear });
    assert!(r.is_blank(), "clearRect of all drops the ops and the base");
}

#[test]
fn edges_are_anti_aliased() {
    // Chrome: fillRect(0.5, 0, 1, 1) in black gives alpha 127 in pixels 0 and 1. fillRect(0, 1.25, 1, 0.5) gives alpha 127 in row 1.
    let mut r = Record::new(3, 3);
    r.push(solid(rect(0.5, 0.0, 1.0, 1.0), 0xFF00_0000));
    r.push(solid(rect(0.0, 1.25, 1.0, 0.5), 0xFF00_0000));
    let px = r.rgba(0, 0, 3, 2);
    assert_eq!([px[3], px[7], px[11]], [127, 127, 0]);
    assert_eq!(px[15], 127);
}

#[test]
fn evenodd_leaves_a_hole() {
    let mut r = Record::new(6, 6);
    let mut path = rect(0.0, 0.0, 6.0, 6.0);
    path.push((f32::NAN, f32::NAN));
    path.extend(rect(2.0, 2.0, 2.0, 2.0));
    r.push(Draw::Fill { path: path.clone(), evenodd: true, src: Source::Solid(0xFF00_0000), alpha: 1.0, mode: Mode::SourceOver });
    let px = r.rgba(0, 0, 6, 6);
    assert_eq!((px[3], px[(2 * 6 + 2) * 4 + 3]), (255, 0));
    // Nonzero fills the hole: both contours turn the same way.
    let mut r = Record::new(6, 6);
    r.push(Draw::Fill { path, evenodd: false, src: Source::Solid(0xFF00_0000), alpha: 1.0, mode: Mode::SourceOver });
    assert_eq!(r.rgba(2, 2, 1, 1)[3], 255);
}

#[test]
fn put_and_read_round_trip_and_premultiply() {
    let mut r = Record::new(2, 1);
    r.push(Draw::Put { x: 0, y: 0, w: 2, h: 1, px: vec![premul(0x80FF_0000), premul(0xFF00_FF00)] });
    // Chrome gives [255, 0, 0, 128] back for an alpha of 128: 255 * 128 / 255 = 128, then 128 * 255 / 128 = 255.
    assert_eq!(r.rgba(0, 0, 2, 1), [255, 0, 0, 128, 0, 255, 0, 255]);
    assert_eq!(r.rgba(-1, 0, 2, 1), [0, 0, 0, 0, 255, 0, 0, 128], "outside the canvas is transparent black");
}

#[test]
fn an_image_scales_with_bilinear_or_nearest_samples() {
    // A 2x1 picture, black and white, drawn into 4x1. Chrome with smoothing: the inner pixels mix (63 and 191), the outer ones clamp.
    let img = Image { w: 2, h: 1, px: Px::Premul(Arc::new(vec![0xFF00_0000, 0xFFFF_FFFF])) };
    let draw = |smooth| {
        let mut r = Record::new(4, 1);
        let inv = Affine([0.5, 0.0, 0.0, 1.0, 0.0, 0.0]);
        r.push(Draw::Fill { path: rect(0.0, 0.0, 4.0, 1.0), evenodd: false, src: Source::Image { img: img.clone(), inv, clamp: (0, 0, 2, 1), smooth }, alpha: 1.0, mode: Mode::SourceOver });
        r.rgba(0, 0, 4, 1).chunks(4).map(|p| p[0]).collect::<Vec<_>>()
    };
    assert_eq!(draw(true), [0, 63, 191, 255]);
    assert_eq!(draw(false), [0, 0, 255, 255]);
}

#[test]
fn a_canvas_source_keeps_its_old_pixels() {
    // The op holds the base of the source. A later draw on the source copies first, so the op still sees the old pixels.
    let mut src = Record::new(1, 1);
    src.push(solid(rect(0.0, 0.0, 1.0, 1.0), 0xFFFF_0000));
    let img = src.image();
    let mut dst = Record::new(1, 1);
    dst.push(Draw::Fill { path: rect(0.0, 0.0, 1.0, 1.0), evenodd: false, src: Source::Image { img, inv: Affine::IDENTITY, clamp: (0, 0, 1, 1), smooth: false }, alpha: 1.0, mode: Mode::SourceOver });
    src.push(solid(rect(0.0, 0.0, 0.5, 1.0), 0xFF00_00FF));
    src.flush();
    assert_eq!(dst.rgba(0, 0, 1, 1), [255, 0, 0, 255]);
}

#[test]
fn a_canvas_over_the_limits_stays_blank() {
    let mut r = Record::new(MAX_SIDE + 1, 1);
    r.push(solid(rect(0.0, 0.0, 1.0, 1.0), 0xFF00_0000));
    assert!(r.is_blank());
}


#[test]
fn a_fractional_rectangle_gets_exact_coverage() {
    // Chrome: fillRect(3.6, 0, 1, 1.4) gives the alphas 102, 154 in row 0 and 40, 61 in row 1. An edge pixel may differ by 1 (Skia
    // rounds coverage in fixed point).
    let mut r = Record::new(6, 2);
    r.push(solid(rect(3.6, 0.0, 1.0, 1.4), 0xFF00_0000));
    let a: Vec<u8> = r.rgba(0, 0, 6, 2).chunks(4).map(|p| p[3]).collect();
    let want = [0, 0, 0, 102, 154, 0, 0, 0, 0, 40, 61, 0];
    assert!(a.iter().zip(want).all(|(&g, w)| g.abs_diff(w) <= 1), "{a:?}");
}
