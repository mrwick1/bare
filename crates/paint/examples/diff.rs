//! Compare two PNG pictures, for the Chrome checks of `docs/resources.md`:
//!
//! - `diff A.png B.png [OUT.png]`: how many pixels differ, how many by more than 64 levels in a channel, the box that
//!   holds them, and (with OUT) a picture: the pixels of A, in red where they differ.
//! - `dump A.png X Y W H`: the pixels of a rectangle as `rrggbb`, one row for each line.
//! - `crop A.png X Y W H SCALE OUT.png`: a rectangle, enlarged with whole-pixel steps, to look at.
//!
//! Both pictures must have the same size. Alpha is ignored.

use std::fs::File;

fn load(path: &str) -> (u32, u32, Vec<[u8; 3]>) {
    let mut reader = png::Decoder::new(std::io::BufReader::new(File::open(path).unwrap_or_else(|e| panic!("{path}: {e}")))).read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().expect("the picture has a size")];
    let info = reader.next_frame(&mut buf).unwrap();
    let n = info.color_type.samples();
    let px = buf[..info.buffer_size()].chunks(n).map(|p| if n >= 3 { [p[0], p[1], p[2]] } else { [p[0]; 3] }).collect();
    (info.width, info.height, px)
}

fn save(path: &str, w: u32, h: u32, px: &[[u8; 3]]) {
    let mut e = png::Encoder::new(std::io::BufWriter::new(File::create(path).unwrap()), w, h);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(&px.concat()).unwrap();
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let num = |i: usize| a[i].parse::<u32>().unwrap();
    match a.first().map(String::as_str) {
        Some("diff") => {
            let (w, h, p) = load(&a[1]);
            let (w2, h2, q) = load(&a[2]);
            if (w, h) != (w2, h2) {
                println!("size differs: {w}x{h} against {w2}x{h2}");
                std::process::exit(2);
            }
            let (mut n, mut big) = (0usize, 0usize);
            let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
            let mut out = p.clone();
            for i in 0..p.len() {
                if p[i] != q[i] {
                    n += 1;
                    big += p[i].iter().zip(&q[i]).any(|(&x, &y)| x.abs_diff(y) > 64) as usize;
                    let (x, y) = (i as u32 % w, i as u32 / w);
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
                    out[i] = [255, 0, 0];
                }
            }
            println!("{n} of {} pixels differ ({big} by more than 64 levels){}", p.len(), if n > 0 { format!(", in x {x0}..{x1}, y {y0}..{y1}") } else { String::new() });
            if let Some(path) = a.get(3) {
                save(path, w, h, &out);
            }
        }
        Some("dump") => {
            let (w, _, p) = load(&a[1]);
            for y in num(3)..num(3) + num(5) {
                let row: Vec<String> = (num(2)..num(2) + num(4)).map(|x| p[(y * w + x) as usize].map(|c| format!("{c:02x}")).concat()).collect();
                println!("{y:4}: {}", row.join(" "));
            }
        }
        Some("crop") => {
            let (w, _, p) = load(&a[1]);
            let (x, y, cw, ch, s) = (num(2), num(3), num(4), num(5), num(6));
            let mut out = Vec::new();
            for j in 0..ch * s {
                for i in 0..cw * s {
                    out.push(p[((y + j / s) * w + x + i / s) as usize]);
                }
            }
            save(&a[7], cw * s, ch * s, &out);
        }
        _ => eprintln!("usage: diff A B [OUT] | dump A X Y W H | crop A X Y W H SCALE OUT"),
    }
}
