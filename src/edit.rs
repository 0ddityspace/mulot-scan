//! Post-processing of a finished scan.

use image::{GrayImage, RgbImage, imageops};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorMode {
    Color,
    Gray,
    /// Adaptive black & white (best for text).
    BlackWhite,
}

/// Crop rectangle in normalised coordinates (0..1) of the rotated image.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CropRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl CropRect {
    pub const FULL: CropRect = CropRect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 };

    pub fn normalized(self) -> Self {
        let c = |v: f32| v.clamp(0.0, 1.0);
        CropRect {
            x0: c(self.x0.min(self.x1)),
            y0: c(self.y0.min(self.y1)),
            x1: c(self.x0.max(self.x1)),
            y1: c(self.y0.max(self.y1)),
        }
    }

    pub fn is_full(&self) -> bool {
        *self == Self::FULL
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Adjustments {
    /// Quarter turns clockwise.
    pub quarter_turns: u8,
    pub mirror: bool,
    /// Fine rotation in degrees (deskew).
    pub angle: f32,
    pub crop: CropRect,
    /// -100 ..= 100
    pub brightness: f32,
    /// -100 ..= 100
    pub contrast: f32,
    pub mode: ColorMode,
    /// Black & white sensitivity, 0..=100.
    pub bw_level: f32,
}

impl Default for Adjustments {
    fn default() -> Self {
        Self {
            quarter_turns: 0,
            mirror: false,
            angle: 0.0,
            crop: CropRect::FULL,
            brightness: 0.0,
            contrast: 0.0,
            mode: ColorMode::Color,
            bw_level: 50.0,
        }
    }
}

impl Adjustments {
    /// Geometry only (rotation, mirror, fine angle) – the frame on which the crop is defined.
    pub fn geometry(&self, src: &RgbImage) -> RgbImage {
        let mut img = match self.quarter_turns % 4 {
            1 => imageops::rotate90(src),
            2 => imageops::rotate180(src),
            3 => imageops::rotate270(src),
            _ => src.clone(),
        };
        if self.mirror {
            imageops::flip_horizontal_in_place(&mut img);
        }
        if self.angle.abs() > 0.01 {
            img = rotate_fine(&img, self.angle.to_radians(), [255, 255, 255]);
        }
        img
    }

    pub fn apply(&self, src: &RgbImage) -> RgbImage {
        let geo = self.geometry(src);
        let c = self.crop.normalized();
        let (w, h) = geo.dimensions();
        let x0 = (c.x0 * w as f32) as u32;
        let y0 = (c.y0 * h as f32) as u32;
        let x1 = ((c.x1 * w as f32) as u32).clamp(x0 + 1, w);
        let y1 = ((c.y1 * h as f32) as u32).clamp(y0 + 1, h);
        let mut img = imageops::crop_imm(&geo, x0, y0, x1 - x0, y1 - y0).to_image();
        tone(&mut img, self.brightness, self.contrast);
        match self.mode {
            ColorMode::Color => img,
            ColorMode::Gray => {
                let g = imageops::grayscale(&img);
                gray_to_rgb(&g)
            }
            ColorMode::BlackWhite => gray_to_rgb(&adaptive_threshold(&imageops::grayscale(&img), self.bw_level)),
        }
    }
}

fn gray_to_rgb(g: &GrayImage) -> RgbImage {
    let raw = g.as_raw().iter().flat_map(|&v| [v, v, v]).collect();
    RgbImage::from_raw(g.width(), g.height(), raw).expect("buffer size matches")
}

fn tone(img: &mut RgbImage, brightness: f32, contrast: f32) {
    if brightness.abs() < 0.5 && contrast.abs() < 0.5 {
        return;
    }
    let b = brightness / 100.0 * 128.0;
    let c = contrast.clamp(-99.0, 100.0) / 100.0;
    let k = if c >= 0.0 { 1.0 + c * 3.0 } else { 1.0 + c };
    let lut: Vec<u8> =
        (0..256).map(|i| (((i as f32 - 128.0) * k + 128.0 + b).round().clamp(0.0, 255.0)) as u8).collect();
    for p in img.pixels_mut() {
        for v in p.0.iter_mut() {
            *v = lut[*v as usize];
        }
    }
}

/// Bilinear rotation around the centre, keeping the original size.
pub fn rotate_fine(src: &RgbImage, angle: f32, bg: [u8; 3]) -> RgbImage {
    let (w, h) = src.dimensions();
    let (s, c) = angle.sin_cos();
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let (umax, vmax) = ((w - 1) as f32, (h - 1) as f32);
    let (raw, stride) = (src.as_raw(), w as usize * 3);
    let mut out = RgbImage::new(w, h);
    // Interactive (runs on every slider move): rows in parallel.
    out.par_chunks_exact_mut(stride).enumerate().for_each(|(y, row)| {
        let dy = y as f32 + 0.5 - cy;
        for (x, px) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let dx = x as f32 + 0.5 - cx;
            let u = c * dx + s * dy + cx - 0.5;
            let v = -s * dx + c * dy + cy - 0.5;
            if u < 0.0 || v < 0.0 || u >= umax || v >= vmax {
                *px = bg;
                continue;
            }
            let (iu, iv) = (u as usize, v as usize);
            let (au, av) = (u - iu as f32, v - iv as f32);
            let i = iv * stride + iu * 3;
            let k = [(1.0 - au) * (1.0 - av), au * (1.0 - av), (1.0 - au) * av, au * av];
            *px = std::array::from_fn(|ch| {
                let p = |o: usize| raw[i + o + ch] as f32;
                (p(0) * k[0] + p(3) * k[1] + p(stride) * k[2] + p(stride + 3) * k[3] + 0.5) as u8
            });
        }
    });
    out
}

/// Local-mean thresholding robust to uneven illumination.
pub fn adaptive_threshold(g: &GrayImage, level: f32) -> GrayImage {
    let (w, h) = g.dimensions();
    let (wu, hu) = (w as usize, h as usize);
    // Integral image.
    let src = g.as_raw();
    let mut integral = vec![0u64; (wu + 1) * (hu + 1)];
    for (y, line) in src.chunks_exact(wu).enumerate() {
        let mut row = 0u64;
        for (x, &v) in line.iter().enumerate() {
            row += v as u64;
            integral[(y + 1) * (wu + 1) + x + 1] = integral[y * (wu + 1) + x + 1] + row;
        }
    }
    let r = ((w.min(h) / 40).max(8)) as usize;
    let k = 0.70 + level.clamp(0.0, 100.0) / 100.0 * 0.25;
    let mut out = GrayImage::new(w, h);
    out.par_chunks_exact_mut(wu).enumerate().for_each(|(y, row)| {
        let (y0, y1) = (y.saturating_sub(r), (y + r + 1).min(hu));
        for (x, o) in row.iter_mut().enumerate() {
            let (x0, x1) = (x.saturating_sub(r), (x + r + 1).min(wu));
            let sum = integral[y1 * (wu + 1) + x1] + integral[y0 * (wu + 1) + x0]
                - integral[y0 * (wu + 1) + x1]
                - integral[y1 * (wu + 1) + x0];
            let mean = sum as f32 / ((x1 - x0) * (y1 - y0)) as f32;
            *o = if (src[y * wu + x] as f32) < mean * k { 0 } else { 255 };
        }
    });
    out
}

/// Estimates the skew angle (degrees) of text lines with a projection profile.
pub fn detect_skew(img: &RgbImage) -> f32 {
    let g = imageops::grayscale(img);
    let scale = (900.0 / g.width().max(g.height()) as f32).min(1.0);
    let small = imageops::resize(
        &g,
        ((g.width() as f32 * scale) as u32).max(1),
        ((g.height() as f32 * scale) as u32).max(1),
        imageops::FilterType::Triangle,
    );
    let bw = adaptive_threshold(&small, 50.0);
    let dark: Vec<(f32, f32)> =
        bw.enumerate_pixels().filter(|(_, _, p)| p.0[0] == 0).map(|(x, y, _)| (x as f32, y as f32)).collect();
    if dark.len() < 50 {
        return 0.0;
    }
    let h = bw.height() as f32;
    let score = |deg: f32| -> f64 {
        let t = deg.to_radians();
        let (s, c) = t.sin_cos();
        let n = (h * 1.5) as usize + 2;
        let mut bins = vec![0u32; n];
        let off = h * 0.25;
        for &(x, y) in &dark {
            let r = y * c - x * s + off;
            if r >= 0.0 && (r as usize) < n {
                bins[r as usize] += 1;
            }
        }
        bins.iter().map(|&b| (b as f64) * (b as f64)).sum()
    };
    let mut best = (0.0f32, f64::MIN);
    let mut a = -10.0;
    while a <= 10.0 {
        let sc = score(a);
        if sc > best.1 {
            best = (a, sc);
        }
        a += 0.5;
    }
    let mut a = best.0 - 0.5;
    while a <= best.0 + 0.5 {
        let sc = score(a);
        if sc > best.1 {
            best = (a, sc);
        }
        a += 0.1;
    }
    // Rotating by -skew straightens the lines.
    -best.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern() -> RgbImage {
        RgbImage::from_fn(64, 48, |x, y| image::Rgb([(x * 4) as u8, (y * 5) as u8, ((x + y) * 2) as u8]))
    }

    #[test]
    fn rotate_fine_by_zero_keeps_the_image() {
        let img = pattern();
        let out = rotate_fine(&img, 0.0, [255, 0, 0]);
        // The last row/column fall outside the interpolation domain and get the background.
        for (x, y, p) in out.enumerate_pixels() {
            let expected = if x == 63 || y == 47 { [255, 0, 0] } else { img.get_pixel(x, y).0 };
            assert_eq!(p.0, expected, "({x}, {y})");
        }
    }

    #[test]
    fn rotate_fine_half_turn_matches_rotate180() {
        let img = pattern();
        let out = rotate_fine(&img, std::f32::consts::PI, [0, 0, 0]);
        let reference = imageops::rotate180(&img);
        // Same pixels away from the border, within interpolation rounding.
        for y in 2..46 {
            for x in 2..62 {
                let (a, b) = (out.get_pixel(x, y).0, reference.get_pixel(x, y).0);
                assert!(a.iter().zip(b).all(|(&a, b)| a.abs_diff(b) <= 1), "({x}, {y}): {a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn adaptive_threshold_separates_text_from_paper() {
        let mut g = GrayImage::from_pixel(200, 100, image::Luma([220]));
        // A 3-pixel stroke, like a letter (large dark areas are not "text").
        for y in 49..52 {
            for x in 50..150 {
                g.put_pixel(x, y, image::Luma([30]));
            }
        }
        let bw = adaptive_threshold(&g, 50.0);
        assert_eq!(bw.get_pixel(100, 50).0[0], 0);
        assert_eq!(bw.get_pixel(10, 10).0[0], 255);
    }
}
