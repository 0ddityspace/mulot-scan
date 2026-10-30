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

/// White-patch white balance + black point stretch, computed on covered pixels.
pub fn auto_white_balance(img: &mut RgbImage, mask: &[bool]) {
    let mut hist = [[0u32; 256]; 3];
    let mut n = 0u32;
    for (p, &m) in img.pixels().zip(mask) {
        if m {
            n += 1;
            for k in 0..3 {
                hist[k][p.0[k] as usize] += 1;
            }
        }
    }
    if n < 100 {
        return;
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
            lut[k][i] = (((i as f32 - lo) / (hi - lo)) * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    for (p, &m) in img.pixels_mut().zip(mask) {
        if m {
            for k in 0..3 {
                p.0[k] = lut[k][p.0[k] as usize];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
