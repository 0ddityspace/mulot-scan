//! Generates a synthetic scanner-mouse recording from a document image.
//!
//! The frames follow a serpentine path with a slight rotation wobble, get a
//! green cast and vignetting similar to the real camera, and are encoded as
//! MJPEG frames carrying the same header layout as the firmware (so that
//! `mulot-scan stitch` and `mulot-scan --replay` can consume them).
//!
//! Usage: cargo run --release --example synth -- document.png out.mjpg

use image::codecs::jpeg::JpegEncoder;
use image::{Rgb, RgbImage};

const W: u32 = 640;
const H: u32 = 296;

fn sample(doc: &RgbImage, x: f64, y: f64) -> [f32; 3] {
    if x < 0.0 || y < 0.0 || x >= (doc.width() - 1) as f64 || y >= (doc.height() - 1) as f64 {
        return [235.0, 232.0, 225.0]; // desk
    }
    let (ix, iy) = (x as u32, y as u32);
    let (ax, ay) = ((x - ix as f64) as f32, (y - iy as f64) as f32);
    let p = |x, y| doc.get_pixel(x, y).0.map(|v| v as f32);
    let (a, b, c, d) = (p(ix, iy), p(ix + 1, iy), p(ix, iy + 1), p(ix + 1, iy + 1));
    [0, 1, 2].map(|k| a[k] * (1.0 - ax) * (1.0 - ay) + b[k] * ax * (1.0 - ay) + c[k] * (1.0 - ax) * ay + d[k] * ax * ay)
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let doc = image::open(args.first().expect("document image"))?.to_rgb8();
    let out = args.get(1).expect("output file");
    let (dw, dh) = (doc.width() as f64, doc.height() as f64);

    // Serpentine path over the document.
    let mut path = Vec::new();
    let rows = ((dh - H as f64 * 0.6) / (H as f64 * 0.55)).ceil().max(1.0) as usize + 1;
    let speed = 11.0;
    for r in 0..rows {
        let y = H as f64 * 0.5 + r as f64 * (dh - H as f64) / (rows - 1).max(1) as f64;
        let (x0, x1) = (W as f64 * 0.5, dw - W as f64 * 0.5);
        let n = ((x1 - x0) / speed) as usize;
        for i in 0..=n {
            let t = i as f64 / n as f64;
            let x = if r % 2 == 0 { x0 + (x1 - x0) * t } else { x1 - (x1 - x0) * t };
            path.push((x, y));
        }
        if r + 1 < rows {
            // Move down to the next row.
            let ny = H as f64 * 0.5 + (r + 1) as f64 * (dh - H as f64) / (rows - 1).max(1) as f64;
            let x = if r % 2 == 0 { x1 } else { x0 };
            let n = ((ny - y) / speed * 1.3) as usize;
            for i in 1..n {
                path.push((x, y + (ny - y) * i as f64 / n as f64));
            }
        }
    }

    let mut file = Vec::new();
    for (i, &(x, y)) in path.iter().enumerate() {
        let theta = (i as f64 * 0.05).sin() * 0.02;
        let (s, c) = theta.sin_cos();
        let frame = RgbImage::from_fn(W, H, |u, v| {
            let (du, dv) = (u as f64 + 0.5 - W as f64 / 2.0, v as f64 + 0.5 - H as f64 / 2.0);
            let px = sample(&doc, c * du - s * dv + x, s * du + c * dv + y);
            // Vignetting + green cast + noise.
            let r2 = (du / (W as f64 / 2.0)).powi(2) * 0.6 + (dv / (H as f64 / 2.0)).powi(2) * 0.4;
            let vig = (1.0 - 0.35 * r2) as f32;
            let tint = [0.62f32, 0.80, 0.42];
            let noise = (((u * 7919 + v * 104729 + i as u32 * 31) % 17) as f32 - 8.0) * 0.6;
            Rgb([0, 1, 2].map(|k| (px[k] * tint[k] * vig + noise).clamp(0.0, 255.0) as u8))
        });
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 85).encode_image(&frame)?;
        // Put the frame counter in the SOF height field, like the firmware does.
        if let Some(p) = jpeg.windows(2).position(|w| w == [0xff, 0xc0]) {
            jpeg[p + 5] = (i % 256) as u8;
        }
        file.extend_from_slice(&jpeg);
    }
    std::fs::write(out, &file)?;
    println!("{} images écrites dans {out}", path.len());
    Ok(())
}
