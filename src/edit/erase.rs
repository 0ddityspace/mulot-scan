//! Eraser: paints over a brush stroke, either with white or with a fill
//! reconstructed from the surroundings.

use image::RgbImage;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EraseMode {
    /// Plain white, for margins and paper backgrounds.
    White,
    /// Fill matching the surrounding colours and paper grain.
    Fill,
}

/// A brush stroke, in coordinates normalised by the source image width and
/// height, so that the same stroke applies to the full image and its preview.
#[derive(Clone, Debug)]
pub struct Stroke {
    pub points: Vec<(f32, f32)>,
    /// Brush radius as a fraction of the image width.
    pub radius: f32,
    pub mode: EraseMode,
}

/// Rectangle of an image as it was before an edit. Swapping it with the image
/// undoes the edit, and swapping again redoes it.
pub struct Patch {
    x: u32,
    y: u32,
    pixels: RgbImage,
}

impl Patch {
    fn save(img: &RgbImage, x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, pixels: image::imageops::crop_imm(img, x, y, w, h).to_image() }
    }

    pub fn swap(&mut self, img: &mut RgbImage) {
        for (px, py, p) in self.pixels.enumerate_pixels_mut() {
            std::mem::swap(&mut p.0, &mut img.get_pixel_mut(self.x + px, self.y + py).0);
        }
    }
}

/// Width of the soft brush edge, in pixels.
const FEATHER: f32 = 1.5;
/// Pixels departing more than this from the smooth background are content
/// (text, lines), not paper grain.
const GRAIN_MAX: f32 = 18.0;
/// Pixels darker than the paper by more than this are content, not background.
const DARK_CONTENT: u32 = 35;

/// Applies `stroke` to `img` and returns what it covered, for undo.
pub fn erase(img: &mut RgbImage, stroke: &Stroke) -> Option<Patch> {
    let (w, h) = img.dimensions();
    let r = (stroke.radius * w as f32).max(1.0);
    let pts: Vec<(f32, f32)> = stroke.points.iter().map(|&(x, y)| (x * w as f32, y * h as f32)).collect();
    // Context used to reconstruct the fill around the stroke.
    let margin = match stroke.mode {
        EraseMode::White => FEATHER,
        EraseMode::Fill => (r * 2.0).max(12.0),
    };
    let reach = r + margin + 1.0;
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(x, y) in &pts {
        (x0, y0, x1, y1) = (x0.min(x - reach), y0.min(y - reach), x1.max(x + reach), y1.max(y + reach));
    }
    let x0 = x0.max(0.0) as u32;
    let y0 = y0.max(0.0) as u32;
    let x1 = (x1.max(0.0) as u32 + 1).min(w);
    let y1 = (y1.max(0.0) as u32 + 1).min(h);
    if pts.is_empty() || x0 >= x1 || y0 >= y1 {
        return None;
    }
    let (rw, rh) = (x1 - x0, y1 - y0);
    let dist = distance_to_stroke(&pts, x0, y0, rw, rh, reach);
    let cover: Vec<f32> = dist.iter().map(|&d| ((r - d) / FEATHER + 0.5).clamp(0.0, 1.0)).collect();
    if cover.iter().all(|&c| c == 0.0) {
        return None;
    }
    let patch = Patch::save(img, x0, y0, rw, rh);
    let fill: Vec<[f32; 3]> = match stroke.mode {
        EraseMode::White => vec![[255.0; 3]; cover.len()],
        EraseMode::Fill => reconstruct(&patch.pixels, &cover, &dist, r + margin * 0.5),
    };
    for (i, (&c, f)) in cover.iter().zip(&fill).enumerate() {
        if c > 0.0 {
            let p = img.get_pixel_mut(x0 + i as u32 % rw, y0 + i as u32 / rw);
            for (v, &f) in p.0.iter_mut().zip(f) {
                *v = (*v as f32 + (f - *v as f32) * c).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Some(patch)
}

/// Distance from each pixel of the region to the stroke polyline, up to `reach`.
fn distance_to_stroke(pts: &[(f32, f32)], x0: u32, y0: u32, w: u32, h: u32, reach: f32) -> Vec<f32> {
    let mut dist = vec![f32::MAX; (w * h) as usize];
    let first = [pts[0]];
    let segments = pts.windows(2).map(|s| (s[0], s[1])).chain(first.iter().map(|&p| (p, p)));
    for (a, b) in segments {
        // Range of region pixels within `reach` of the segment, along one axis.
        let span = |lo: f32, hi: f32, origin: u32, size: u32| {
            let start = ((lo - reach).floor() - origin as f32).clamp(0.0, size as f32) as u32;
            let end = ((hi + reach).ceil() - origin as f32 + 1.0).clamp(0.0, size as f32) as u32;
            start..end
        };
        let xs = span(a.0.min(b.0), a.0.max(b.0), x0, w);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        for y in span(a.1.min(b.1), a.1.max(b.1), y0, h) {
            let py = (y + y0) as f32 + 0.5;
            for x in xs.clone() {
                let px = (x + x0) as f32 + 0.5;
                let t = if len2 > 0.0 { (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                let (ex, ey) = (px - a.0 - t * dx, py - a.1 - t * dy);
                let d = &mut dist[(y * w + x) as usize];
                *d = d.min((ex * ex + ey * ey).sqrt());
            }
        }
    }
    dist
}

/// Fills the covered pixels from their surroundings: a smooth interpolation of
/// the known colours (push-pull over an image pyramid), plus grain sampled
/// from the paper around the stroke so that the fill does not look flat.
fn reconstruct(region: &RgbImage, cover: &[f32], dist: &[f32], ring: f32) -> Vec<[f32; 3]> {
    let (w, h) = (region.width() as usize, region.height() as usize);
    let luma = |p: &image::Rgb<u8>| (p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8;
    // What the stroke cut through (a line, the rest of a blot) continues under
    // its edge: such dark pixels would smear into the fill, so only the
    // background is used as source — unless there is little of it.
    let mut lumas: Vec<u32> = region.pixels().zip(cover).filter(|(_, c)| **c == 0.0).map(|(p, _)| luma(p)).collect();
    let known = lumas.len();
    let mid = known / 2;
    let paper = if known > 0 { *lumas.select_nth_unstable(mid).1 } else { 255 };
    let dark = |p: &image::Rgb<u8>| luma(p) + DARK_CONTENT < paper;
    let background = region.pixels().zip(cover).filter(|(p, c)| **c == 0.0 && !dark(p)).count();
    let skip_dark = background * 10 >= known * 3;
    let base: Vec<[f32; 4]> =
        region
            .pixels()
            .zip(cover)
            .map(|(p, &c)| {
                if c > 0.0 || (skip_dark && dark(p)) { [0.0; 4] } else { [p[0] as f32, p[1] as f32, p[2] as f32, 1.0] }
            })
            .collect();
    if !base.iter().any(|p| p[3] > 0.0) {
        return vec![[255.0; 3]; w * h];
    }
    // Push: average the known pixels down to a single one.
    let mut levels = vec![(w, h, base)];
    while let Some((lw, lh, l)) = levels.last()
        && (*lw > 1 || *lh > 1)
    {
        let (nw, nh) = (lw.div_ceil(2), lh.div_ceil(2));
        let mut next = vec![[0f32; 4]; nw * nh];
        for (i, p) in l.iter().enumerate() {
            let n = &mut next[(i / lw / 2) * nw + (i % lw) / 2];
            for c in 0..4 {
                n[c] += p[c];
            }
        }
        for n in &mut next {
            if n[3] > 0.0 {
                *n = n.map(|v| v / n[3]);
            }
        }
        levels.push((nw, nh, next));
    }
    // Pull: fill each level's holes from the (already filled) coarser level.
    for k in (0..levels.len() - 1).rev() {
        let (upper, lower) = levels.split_at_mut(k + 1);
        let (lw, _, l) = &mut upper[k];
        let coarse = &lower[0];
        for (i, p) in l.iter_mut().enumerate() {
            if p[3] < 1.0 {
                let c = sample(coarse, ((i % *lw) as f32 + 0.5) / 2.0 - 0.5, ((i / *lw) as f32 + 0.5) / 2.0 - 0.5);
                *p = [c[0], c[1], c[2], 1.0];
            }
        }
    }
    let smooth = &levels[0].2;
    // Grain: deviation of the paper around the stroke from its smoothed version.
    let mut grain: Vec<[f32; 3]> = Vec::new();
    if levels.len() > 1 {
        for (i, &d) in dist.iter().enumerate() {
            if cover[i] == 0.0 && d < ring {
                let s = sample(&levels[1], ((i % w) as f32 + 0.5) / 2.0 - 0.5, ((i / w) as f32 + 0.5) / 2.0 - 0.5);
                let g: [f32; 3] = std::array::from_fn(|c| smooth[i][c] - s[c]);
                if g.iter().all(|v| v.abs() < GRAIN_MAX) {
                    grain.push(g);
                }
            }
        }
    }
    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    smooth
        .iter()
        .zip(cover)
        .map(|(p, &c)| {
            if c == 0.0 || grain.is_empty() {
                return [p[0], p[1], p[2]];
            }
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let g = grain[(rng % grain.len() as u64) as usize];
            std::array::from_fn(|ch| p[ch] + g[ch])
        })
        .collect()
}

/// Bilinear sample of a pyramid level (clamped at the borders).
fn sample(level: &(usize, usize, Vec<[f32; 4]>), x: f32, y: f32) -> [f32; 3] {
    let (w, h, l) = level;
    let x = x.clamp(0.0, (*w - 1) as f32);
    let y = y.clamp(0.0, (*h - 1) as f32);
    let (ix, iy) = (x as usize, y as usize);
    let (ax, ay) = (x - ix as f32, y - iy as f32);
    let (jx, jy) = ((ix + 1).min(w - 1), (iy + 1).min(h - 1));
    let at = |x: usize, y: usize| l[y * w + x];
    std::array::from_fn(|c| {
        let top = at(ix, iy)[c] * (1.0 - ax) + at(jx, iy)[c] * ax;
        let bottom = at(ix, jy)[c] * (1.0 - ax) + at(jx, jy)[c] * ax;
        top * (1.0 - ay) + bottom * ay
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    /// Paper with a horizontal shading, a black blot in the middle.
    fn page() -> RgbImage {
        RgbImage::from_fn(200, 120, |x, y| {
            if (90..110).contains(&x) && (50..70).contains(&y) {
                Rgb([0, 0, 0])
            } else {
                let v = 170 + (x / 4) as u8;
                Rgb([v, v - 5, v - 10])
            }
        })
    }

    fn stroke(mode: EraseMode) -> Stroke {
        Stroke { points: vec![(0.47, 0.5), (0.53, 0.5)], radius: 0.08, mode }
    }

    #[test]
    fn white_eraser_paints_white_and_undo_restores() {
        let original = page();
        let mut img = original.clone();
        let mut patch = erase(&mut img, &stroke(EraseMode::White)).unwrap();
        assert_eq!(img.get_pixel(100, 60).0, [255, 255, 255]);
        assert_eq!(img.get_pixel(10, 10), original.get_pixel(10, 10));
        patch.swap(&mut img);
        assert_eq!(img, original);
        // Swapping again redoes the edit.
        patch.swap(&mut img);
        assert_eq!(img.get_pixel(100, 60).0, [255, 255, 255]);
    }

    #[test]
    fn fill_eraser_does_not_smear_a_line_it_cuts() {
        // Light paper crossed by a dark ruled line; erase a stretch of the line.
        // The line is slightly slanted, as on a scan that is not straightened.
        let mut img = RgbImage::from_fn(200, 120, |x, y| {
            let line = 52.0 + x as f32 * 0.08;
            if (y as f32 - line).abs() < 2.0 { Rgb([20; 3]) } else { Rgb([230; 3]) }
        });
        let stroke = Stroke { points: vec![(0.3, 0.5), (0.7, 0.5)], radius: 0.04, mode: EraseMode::Fill };
        erase(&mut img, &stroke).unwrap();
        let darkest = (62..138).map(|x| img.get_pixel(x, 60).0[0]).min().unwrap();
        assert!(darkest > 225, "{darkest}");
    }

    #[test]
    fn fill_eraser_blends_into_the_shaded_paper() {
        let mut img = page();
        erase(&mut img, &stroke(EraseMode::Fill)).unwrap();
        // The blot is gone and the shading continues through it.
        for x in [92, 100, 108] {
            let expected = 170.0 + (x / 4) as f32;
            let got = img.get_pixel(x, 60).0[0] as f32;
            assert!((got - expected).abs() < 6.0, "x={x}: {got} vs {expected}");
        }
        assert!(img.get_pixel(100, 60).0[2] < img.get_pixel(100, 60).0[0]);
    }
}
