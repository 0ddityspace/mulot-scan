//! Unbounded tiled mosaic with weighted blending.

use std::collections::HashMap;

use image::{Rgb, RgbImage, Rgba, RgbaImage};

use super::register::Gray;

pub const TILE: i32 = 256;

/// Rigid pose of a frame on the canvas: canvas = R(theta) * (p - c) + (x, y),
/// where `p` is a frame pixel and `c` the frame centre.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pose {
    pub x: f64,
    pub y: f64,
    pub theta: f64,
}

impl Pose {
    pub fn new(x: f64, y: f64, theta: f64) -> Self {
        Self { x, y, theta }
    }

    /// Maps a point given relative to the frame centre to canvas coordinates.
    #[inline]
    pub fn apply(&self, u: f64, v: f64) -> (f64, f64) {
        let (s, c) = self.theta.sin_cos();
        (c * u - s * v + self.x, s * u + c * v + self.y)
    }

    /// Maps a canvas point back to coordinates relative to the frame centre.
    #[inline]
    pub fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let (s, c) = self.theta.sin_cos();
        let (dx, dy) = (x - self.x, y - self.y);
        (c * dx + s * dy, -s * dx + c * dy)
    }

    /// Composes a displacement expressed in the frame's own axes.
    pub fn moved_local(&self, du: f64, dv: f64, dtheta: f64) -> Pose {
        let (s, c) = self.theta.sin_cos();
        Pose { x: self.x + c * du - s * dv, y: self.y + s * du + c * dv, theta: self.theta + dtheta }
    }
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
struct Px {
    rgb: [u8; 3],
    _pad: u8,
    w: f32,
}

struct Tile {
    px: Vec<Px>,
}

impl Tile {
    fn new() -> Self {
        Self { px: vec![Px::default(); (TILE * TILE) as usize] }
    }
}

/// Integer axis-aligned bounds (inclusive min, exclusive max).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Bounds {
    pub fn width(&self) -> u32 {
        (self.x1 - self.x0).max(0) as u32
    }
    pub fn height(&self) -> u32 {
        (self.y1 - self.y0).max(0) as u32
    }
}

pub struct Canvas {
    tiles: HashMap<(i32, i32), Tile>,
    bounds: Option<Bounds>,
    /// Maximum accumulated weight; keeps later frames able to refine the result.
    pub max_weight: f32,
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new()
    }
}

impl Canvas {
    pub fn new() -> Self {
        Self { tiles: HashMap::new(), bounds: None, max_weight: 4.0 }
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
        self.bounds = None;
    }

    pub fn is_empty(&self) -> bool {
        self.bounds.is_none()
    }

    pub fn bounds(&self) -> Option<Bounds> {
        self.bounds
    }

    #[inline]
    fn get(&self, x: i32, y: i32) -> Option<&Px> {
        let (tx, ty) = (x.div_euclid(TILE), y.div_euclid(TILE));
        let t = self.tiles.get(&(tx, ty))?;
        let (lx, ly) = (x.rem_euclid(TILE), y.rem_euclid(TILE));
        let p = &t.px[(ly * TILE + lx) as usize];
        if p.w > 0.0 { Some(p) } else { None }
    }

    /// Bilinear luminance lookup; returns (value, weight-coverage).
    #[inline]
    fn sample_luma(&self, x: f64, y: f64) -> Option<f32> {
        let (fx, fy) = (x.floor(), y.floor());
        let (ax, ay) = ((x - fx) as f32, (y - fy) as f32);
        let (ix, iy) = (fx as i32, fy as i32);
        let mut acc = 0.0;
        let mut wsum = 0.0;
        for (dx, dy, w) in
            [(0, 0, (1.0 - ax) * (1.0 - ay)), (1, 0, ax * (1.0 - ay)), (0, 1, (1.0 - ax) * ay), (1, 1, ax * ay)]
        {
            if let Some(p) = self.get(ix + dx, iy + dy) {
                acc += luma(p.rgb) * w;
                wsum += w;
            }
        }
        if wsum > 0.5 { Some(acc / wsum) } else { None }
    }

    /// Blends `img` at `pose`. `weights` holds one weight per frame pixel.
    pub fn blend(&mut self, img: &RgbImage, weights: &[f32], pose: Pose) {
        let (w, h) = (img.width() as i32, img.height() as i32);
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let corners = [(-cx, -cy), (cx, -cy), (-cx, cy), (cx, cy)].map(|(u, v)| pose.apply(u, v));
        let x0 = corners.iter().map(|c| c.0).fold(f64::MAX, f64::min).floor() as i32;
        let x1 = corners.iter().map(|c| c.0).fold(f64::MIN, f64::max).ceil() as i32 + 1;
        let y0 = corners.iter().map(|c| c.1).fold(f64::MAX, f64::min).floor() as i32;
        let y1 = corners.iter().map(|c| c.1).fold(f64::MIN, f64::max).ceil() as i32 + 1;
        let (s, c) = pose.theta.sin_cos();
        let raw = img.as_raw();
        let max_w = self.max_weight;
        let mut touched = false;
        for ty in y0.div_euclid(TILE)..=(y1 - 1).div_euclid(TILE) {
            for tx in x0.div_euclid(TILE)..=(x1 - 1).div_euclid(TILE) {
                let gx0 = (tx * TILE).max(x0);
                let gx1 = ((tx + 1) * TILE).min(x1);
                let gy0 = (ty * TILE).max(y0);
                let gy1 = ((ty + 1) * TILE).min(y1);
                let t = self.tiles.entry((tx, ty)).or_insert_with(Tile::new);
                for gy in gy0..gy1 {
                    for gx in gx0..gx1 {
                        let (dx, dy) = (gx as f64 + 0.5 - pose.x, gy as f64 + 0.5 - pose.y);
                        let u = c * dx + s * dy + cx - 0.5;
                        let v = -s * dx + c * dy + cy - 0.5;
                        if u < 0.0 || v < 0.0 || u > (w - 1) as f64 || v > (h - 1) as f64 {
                            continue;
                        }
                        let (iu, iv) = (u as i32, v as i32);
                        let (au, av) = ((u - iu as f64) as f32, (v - iv as f64) as f32);
                        let iu1 = (iu + 1).min(w - 1);
                        let iv1 = (iv + 1).min(h - 1);
                        let idx = |x: i32, y: i32| (y * w + x) as usize;
                        let wpx = weights[idx(iu, iv)] * (1.0 - au) * (1.0 - av)
                            + weights[idx(iu1, iv)] * au * (1.0 - av)
                            + weights[idx(iu, iv1)] * (1.0 - au) * av
                            + weights[idx(iu1, iv1)] * au * av;
                        if wpx <= 1e-4 {
                            continue;
                        }
                        let mut rgb = [0f32; 3];
                        for (k, ch) in rgb.iter_mut().enumerate() {
                            let p = |x: i32, y: i32| raw[idx(x, y) * 3 + k] as f32;
                            *ch = p(iu, iv) * (1.0 - au) * (1.0 - av)
                                + p(iu1, iv) * au * (1.0 - av)
                                + p(iu, iv1) * (1.0 - au) * av
                                + p(iu1, iv1) * au * av;
                        }
                        let px = &mut t.px[((gy - ty * TILE) * TILE + (gx - tx * TILE)) as usize];
                        let total = px.w + wpx;
                        let a = wpx / total;
                        for k in 0..3 {
                            let old = px.rgb[k] as f32;
                            px.rgb[k] = (old + (rgb[k] - old) * a).round().clamp(0.0, 255.0) as u8;
                        }
                        px.w = total.min(max_w);
                        touched = true;
                    }
                }
            }
        }
        if touched {
            let nb = Bounds { x0, y0, x1, y1 };
            self.bounds = Some(match self.bounds {
                None => nb,
                Some(b) => Bounds { x0: b.x0.min(x0), y0: b.y0.min(y0), x1: b.x1.max(x1), y1: b.y1.max(y1) },
            });
        }
    }

    /// Renders the canvas luminance as seen by a frame at `pose`, downsampled by `step`.
    /// Returns the image and the fraction of covered pixels. Uncovered pixels get the mean.
    pub fn sample_gray(&self, pose: Pose, frame_w: u32, frame_h: u32, step: u32) -> (Gray, f32) {
        let (gw, gh) = ((frame_w / step) as usize, (frame_h / step) as usize);
        let mut g = Gray::new(gw, gh);
        let mut mask = vec![false; gw * gh];
        let (cx, cy) = (frame_w as f64 / 2.0, frame_h as f64 / 2.0);
        let half = step as f64 / 2.0;
        let mut n = 0usize;
        let mut sum = 0.0f32;
        for y in 0..gh {
            for x in 0..gw {
                let u = x as f64 * step as f64 + half - cx;
                let v = y as f64 * step as f64 + half - cy;
                let (px, py) = pose.apply(u, v);
                if let Some(l) = self.sample_luma(px - 0.5, py - 0.5) {
                    g.px[y * gw + x] = l;
                    mask[y * gw + x] = true;
                    n += 1;
                    sum += l;
                }
            }
        }
        let mean = if n > 0 { sum / n as f32 } else { 0.0 };
        for (v, m) in g.px.iter_mut().zip(&mask) {
            if !*m {
                *v = mean;
            }
        }
        (g, n as f32 / (gw * gh) as f32)
    }

    /// Renders the whole canvas (or `region`) at `scale` (<= 1.0 downsamples).
    pub fn render(&self, region: Option<Bounds>, scale: f32, background: [u8; 4]) -> Option<RgbaImage> {
        let b = region.or(self.bounds)?;
        let ow = ((b.width() as f32 * scale).ceil() as u32).max(1);
        let oh = ((b.height() as f32 * scale).ceil() as u32).max(1);
        let mut out = RgbaImage::from_pixel(ow, oh, Rgba(background));
        let inv = 1.0 / scale;
        for oy in 0..oh {
            for ox in 0..ow {
                let x = b.x0 + ((ox as f32 + 0.5) * inv) as i32;
                let y = b.y0 + ((oy as f32 + 0.5) * inv) as i32;
                if let Some(p) = self.get(x, y) {
                    out.put_pixel(ox, oy, Rgba([p.rgb[0], p.rgb[1], p.rgb[2], 255]));
                }
            }
        }
        Some(out)
    }

    /// Full-resolution RGB export with the given background colour.
    pub fn render_rgb(&self, background: [u8; 3]) -> Option<RgbImage> {
        let b = self.bounds?;
        let mut out = RgbImage::from_pixel(b.width(), b.height(), Rgb(background));
        for ((tx, ty), tile) in &self.tiles {
            for ly in 0..TILE {
                let y = ty * TILE + ly - b.y0;
                if y < 0 || y >= b.height() as i32 {
                    continue;
                }
                for lx in 0..TILE {
                    let x = tx * TILE + lx - b.x0;
                    if x < 0 || x >= b.width() as i32 {
                        continue;
                    }
                    let p = &tile.px[(ly * TILE + lx) as usize];
                    if p.w > 0.0 {
                        out.put_pixel(x as u32, y as u32, Rgb(p.rgb));
                    }
                }
            }
        }
        Some(out)
    }

    /// Coverage mask (true where at least one frame contributed), full resolution.
    pub fn coverage(&self) -> Option<(Bounds, Vec<bool>)> {
        let b = self.bounds?;
        let (w, h) = (b.width() as i32, b.height() as i32);
        let mut m = vec![false; (w * h) as usize];
        for ((tx, ty), tile) in &self.tiles {
            for ly in 0..TILE {
                let y = ty * TILE + ly - b.y0;
                if y < 0 || y >= h {
                    continue;
                }
                for lx in 0..TILE {
                    let x = tx * TILE + lx - b.x0;
                    if x >= 0 && x < w && tile.px[(ly * TILE + lx) as usize].w > 0.0 {
                        m[(y * w + x) as usize] = true;
                    }
                }
            }
        }
        Some((b, m))
    }
}

#[inline]
pub fn luma(rgb: [u8; 3]) -> f32 {
    0.299 * rgb[0] as f32 + 0.587 * rgb[1] as f32 + 0.114 * rgb[2] as f32
}
