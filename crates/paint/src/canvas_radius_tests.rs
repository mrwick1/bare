//! A huge or infinite corner radius (`border-radius: 3.40282e38px`, Tailwind 4 `rounded-full`) is cut to fit the box: a pill, not a square.
use crate::canvas::{Canvas, IRect, Shape};

#[test]
fn a_huge_radius_makes_a_pill() {
    let r = IRect { x0: 0, y0: 0, x1: 120, y1: 40 };
    for v in [f32::MAX, f32::INFINITY] {
        let s = Shape::new(r, [v; 8]);
        assert!(s.rad.iter().all(|&x| (x - 20.0).abs() < 0.01), "{v}: {:?}", s.rad);
    }
    let mut c = Canvas::new(120, 40);
    c.fill_shape(&Shape::new(r, [f32::MAX; 8]), 0xff00_0000, c.bounds());
    assert_eq!(c.pixel(60, 20), 0xff00_0000);
    assert_eq!(c.pixel(1, 1), 0xffff_ffff, "the corner is round");
}
