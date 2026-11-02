//! Final image: orientation and white balance.

use image::RgbImage;

/// Maps a point of an image of size `w`×`h` after `q` clockwise quarter turns.
pub fn orient_point(q: u8, x: f64, y: f64, w: f64, h: f64) -> (f64, f64) {
    match q % 4 {
        1 => (h - y, x),
        2 => (w - x, h - y),
        3 => (y, w - x),
        _ => (x, y),
    }
}

/// Rotates an image by `q` clockwise quarter turns.
pub fn orient_image<P: image::Pixel + 'static>(
    q: u8,
    img: &image::ImageBuffer<P, Vec<P::Subpixel>>,
) -> image::ImageBuffer<P, Vec<P::Subpixel>> {
    use image::imageops;
    match q % 4 {
        1 => imageops::rotate90(img),
        2 => imageops::rotate180(img),
        3 => imageops::rotate270(img),
        _ => img.clone(),
    }
}

/// Part of the image to keep without the irregular, unscanned border: the
/// bounds of `mask`, shrunk until each edge is almost entirely scanned.
/// Returns (x0, y0, x1, y1) as fractions of the image.
pub fn scanned_area(mask: &image::GrayImage) -> Option<[f32; 4]> {
    const MIN_EDGE_COVERAGE: f32 = 0.75;
    let (w, h) = (mask.width() as usize, mask.height() as usize);
    // Integral image of the covered pixels.
    let mut sum = vec![0u32; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0;
        for x in 0..w {
            row += (mask.as_raw()[y * w + x] > 0) as u32;
            sum[(y + 1) * (w + 1) + x + 1] = sum[y * (w + 1) + x + 1] + row;
        }
    }
    let count = |x0: usize, y0: usize, x1: usize, y1: usize| {
        sum[y1 * (w + 1) + x1] + sum[y0 * (w + 1) + x0] - sum[y0 * (w + 1) + x1] - sum[y1 * (w + 1) + x0]
    };
    if count(0, 0, w, h) == 0 {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (0, 0, w, h);
    // Moves inward, one pixel at a time, the edge that is the least covered.
    while x1 - x0 > 2 && y1 - y0 > 2 {
        let (cw, ch) = ((x1 - x0) as f32, (y1 - y0) as f32);
        let edges = [
            count(x0, y0, x1, y0 + 1) as f32 / cw,
            count(x0, y1 - 1, x1, y1) as f32 / cw,
            count(x0, y0, x0 + 1, y1) as f32 / ch,
            count(x1 - 1, y0, x1, y1) as f32 / ch,
        ];
        let (i, &worst) = edges.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1))?;
        if worst >= MIN_EDGE_COVERAGE {
            break;
        }
        match i {
            0 => y0 += 1,
            1 => y1 -= 1,
            2 => x0 += 1,
            _ => x1 -= 1,
        }
    }
    Some([x0 as f32 / w as f32, y0 as f32 / h as f32, x1 as f32 / w as f32, y1 as f32 / h as f32])
}

/// Per-channel lookup tables of the automatic levels.
pub type Levels = [[u8; 256]; 3];

/// Tone curve of the original software for this sensor ("LSM100" table of its
/// gamma enhancer). The sensor is linear: once black is at 0 and the paper at
/// full scale, this curve lifts the mid-tones (128 → 169) for a natural look.
#[rustfmt::skip]
const TONE_CURVE: [u8; 256] = [
    0, 3, 7, 10, 13, 17, 21, 25, 28, 31, 33, 36, 39, 42, 44, 47,
    50, 52, 53, 55, 57, 58, 60, 62, 63, 65, 67, 68, 70, 72, 73, 75,
    77, 78, 80, 82, 83, 85, 87, 88, 90, 91, 92, 93, 95, 96, 97, 98,
    99, 100, 101, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115,
    116, 117, 118, 119, 120, 121, 122, 123, 123, 124, 125, 126, 127, 128, 129, 130,
    131, 132, 133, 134, 135, 135, 136, 137, 138, 139, 140, 141, 142, 142, 143, 144,
    145, 146, 147, 147, 148, 149, 150, 150, 151, 152, 153, 153, 154, 155, 156, 156,
    157, 158, 159, 159, 160, 161, 162, 162, 163, 164, 165, 165, 166, 167, 168, 168,
    169, 170, 171, 171, 172, 173, 174, 174, 175, 176, 177, 177, 178, 179, 180, 180,
    181, 182, 182, 183, 184, 184, 185, 186, 186, 187, 188, 188, 189, 190, 190, 191,
    192, 192, 193, 193, 194, 195, 196, 196, 197, 198, 198, 199, 200, 200, 201, 202,
    202, 203, 204, 204, 205, 206, 207, 207, 208, 209, 209, 210, 211, 211, 212, 213,
    214, 214, 215, 216, 217, 217, 218, 219, 220, 220, 221, 222, 223, 223, 224, 225,
    226, 226, 227, 228, 228, 229, 230, 230, 231, 232, 232, 233, 234, 235, 235, 236,
    237, 237, 238, 239, 239, 240, 241, 241, 242, 242, 243, 243, 244, 245, 245, 246,
    246, 247, 248, 248, 249, 249, 250, 250, 251, 252, 252, 253, 253, 254, 254, 255,
];

/// White-patch white balance + black point stretch, then the tone curve,
/// computed on covered pixels.
pub fn auto_white_balance(img: &mut RgbImage, mask: &[bool]) {
    let Some(lut) = levels(img.pixels().zip(mask).filter(|(_, m)| **m).map(|(p, _)| p.0)) else { return };
    for (p, &m) in img.pixels_mut().zip(mask) {
        if m {
            apply_levels(&lut, &mut p.0);
        }
    }
}

/// Automatic levels (white balance on the paper, black point, tone curve) of a
/// set of pixels; `None` if there are too few.
pub fn levels(pixels: impl Iterator<Item = [u8; 3]>) -> Option<Levels> {
    let mut hist = [[0u32; 256]; 3];
    let mut n = 0u32;
    for p in pixels {
        n += 1;
        for k in 0..3 {
            hist[k][p[k] as usize] += 1;
        }
    }
    if n < 100 {
        return None;
    }
    let pct = |h: &[u32; 256], q: f32| -> f32 {
        let target = (n as f32 * q) as u32;
        let mut acc = 0;
        for (i, &c) in h.iter().enumerate() {
            acc += c;
            if acc >= target {
                return i as f32;
            }
        }
        255.0
    };
    let mut lut = [[0u8; 256]; 3];
    for k in 0..3 {
        let lo = pct(&hist[k], 0.005);
        let hi = pct(&hist[k], 0.97).max(lo + 16.0);
        for i in 0..256 {
            let stretched = (((i as f32 - lo) / (hi - lo)) * 255.0).round().clamp(0.0, 255.0) as u8;
            lut[k][i] = TONE_CURVE[stretched as usize];
        }
    }
    Some(lut)
}

pub fn apply_levels(lut: &Levels, rgb: &mut [u8]) {
    for (k, v) in rgb.iter_mut().take(3).enumerate() {
        *v = lut[k][*v as usize];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanned_area_trims_ragged_border() {
        // Covered: a 60×40 block with a notch and a narrow strip sticking out.
        let mask = image::GrayImage::from_fn(100, 80, |x, y| {
            let inside = (20..80).contains(&x) && (20..60).contains(&y) && !(x < 25 && y < 30);
            let strip = (40..45).contains(&x) && y < 20;
            image::Luma([if inside || strip { 255 } else { 0 }])
        });
        let [x0, y0, x1, y1] = scanned_area(&mask).unwrap();
        let px = |v: f32, n: f32| (v * n).round() as i32;
        assert_eq!((px(x1, 100.0), px(y1, 80.0)), (80, 60));
        // The strip is trimmed; a small notch is tolerated.
        assert_eq!((px(x0, 100.0), px(y0, 80.0)), (20, 20));
    }
    use image::Rgb;

    #[test]
    fn orientation_helpers_agree() {
        let img = RgbImage::from_fn(5, 3, |x, y| Rgb([x as u8, y as u8, 0]));
        for q in 0..4u8 {
            let r = orient_image(q, &img);
            let (nx, ny) = orient_point(q, 1.5, 0.5, 5.0, 3.0);
            let p = r.get_pixel(nx.floor() as u32, ny.floor() as u32).0;
            assert_eq!((p[0], p[1]), (1, 0), "q={q}");
        }
    }
}
