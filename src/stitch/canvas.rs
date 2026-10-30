//! Unbounded tiled mosaic with weighted blending.

use image::{Rgb, RgbImage, Rgba, RgbaImage};
use rayon::prelude::*;

use super::register::Gray;

pub const TILE: i32 = 128;

/// Rigid pose of a frame on the canvas: canvas = R(theta) * (p - c) + (x, y),
/// where `p` is a frame pixel and `c` the frame centre.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pose {
    pub x: f64,
    pub y: f64,
    pub theta: f64,
}

impl Pose {
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
struct Px {
    rgb: [u8; 3],
    /// Cached luminance of `rgb`, used by registration.
    luma: u8,
    /// Accumulated blending weight; 0 means "never covered".
    w: f32,
}

struct Tile {
    px: Box<[Px]>,
}

impl Tile {
    fn new() -> Self {
        Self { px: vec![Px::default(); (TILE * TILE) as usize].into_boxed_slice() }
    }
}

/// Dense, growable grid of tiles. Lookups are a bounds check and an index,
/// which matters because registration samples the canvas ~200k times a frame.
#[derive(Default)]
struct TileGrid {
    /// Tile coordinates of `tiles[0]`.
    tx0: i32,
    ty0: i32,
    cols: i32,
    rows: i32,
    tiles: Vec<Option<Tile>>,
}

impl TileGrid {
    #[inline]
    fn get(&self, tx: i32, ty: i32) -> Option<&Tile> {
        let (cx, cy) = (tx - self.tx0, ty - self.ty0);
        if cx < 0 || cy < 0 || cx >= self.cols || cy >= self.rows {
            return None;
        }
        self.tiles[(cy * self.cols + cx) as usize].as_ref()
    }

    /// Returns the tile, creating it (and growing the grid) if needed.
    fn get_or_insert(&mut self, tx: i32, ty: i32) -> &mut Tile {
        let (cx, cy) = (tx - self.tx0, ty - self.ty0);
        if self.tiles.is_empty() || cx < 0 || cy < 0 || cx >= self.cols || cy >= self.rows {
            self.grow_to(tx, ty);
        }
        let i = ((ty - self.ty0) * self.cols + (tx - self.tx0)) as usize;
        self.tiles[i].get_or_insert_with(Tile::new)
    }

    fn grow_to(&mut self, tx: i32, ty: i32) {
        // Grow by a few tiles at once to amortise the re-layout.
        const SLACK: i32 = 2;
        let (x0, y0, x1, y1) = if self.tiles.is_empty() {
            (tx, ty, tx + 1, ty + 1)
        } else {
            (
                self.tx0.min(tx - SLACK),
                self.ty0.min(ty - SLACK),
                (self.tx0 + self.cols).max(tx + 1 + SLACK),
                (self.ty0 + self.rows).max(ty + 1 + SLACK),
            )
        };
        let (cols, rows) = (x1 - x0, y1 - y0);
        let mut tiles: Vec<Option<Tile>> = (0..cols * rows).map(|_| None).collect();
        for (i, t) in std::mem::take(&mut self.tiles).into_iter().enumerate() {
            if t.is_some() {
                let (ox, oy) = (i as i32 % self.cols + self.tx0, i as i32 / self.cols + self.ty0);
                tiles[((oy - y0) * cols + (ox - x0)) as usize] = t;
            }
        }
        *self = Self { tx0: x0, ty0: y0, cols, rows, tiles };
    }

    /// Existing tiles with their tile coordinates.
    fn iter(&self) -> impl Iterator<Item = ((i32, i32), &Tile)> {
        self.tiles.iter().enumerate().filter_map(|(i, t)| {
            let (cx, cy) = (i as i32 % self.cols, i as i32 / self.cols);
            t.as_ref().map(|t| ((cx + self.tx0, cy + self.ty0), t))
        })
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

/// One frame to blend into the canvas, tile by tile.
struct BlendJob<'a> {
    img: &'a RgbImage,
    weights: &'a [f32],
    pose: Pose,
    /// Canvas area covered by the frame.
    area: Bounds,
    max_weight: f32,
}

impl BlendJob<'_> {
    /// Blends the frame into tile (`tx`, `ty`); returns whether a pixel was written.
    fn run(&self, t: &mut Tile, tx: i32, ty: i32) -> bool {
        let (w, h) = (self.img.width() as usize, self.img.height() as usize);
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let (umax, vmax) = ((w - 1) as f64, (h - 1) as f64);
        let (s, c) = self.pose.theta.sin_cos();
        let (raw, weights) = (self.img.as_raw(), self.weights);
        let a = self.area;
        let (gx0, gx1) = ((tx * TILE).max(a.x0), ((tx + 1) * TILE).min(a.x1));
        let (gy0, gy1) = ((ty * TILE).max(a.y0), ((ty + 1) * TILE).min(a.y1));
        let mut touched = false;
        for gy in gy0..gy1 {
            // Frame coordinates of canvas pixel (gx0, gy), then one step per pixel.
            let (dx, dy) = (gx0 as f64 + 0.5 - self.pose.x, gy as f64 + 0.5 - self.pose.y);
            let mut u = c * dx + s * dy + cx - 0.5;
            let mut v = -s * dx + c * dy + cy - 0.5;
            let row = &mut t.px[((gy - ty * TILE) * TILE) as usize..][..TILE as usize];
            for px in &mut row[(gx0 - tx * TILE) as usize..(gx1 - tx * TILE) as usize] {
                let (fu, fv) = (u, v);
                u += c;
                v -= s;
                if fu < 0.0 || fv < 0.0 || fu > umax || fv > vmax {
                    continue;
                }
                // Through i32: float to unsigned conversions are slow on x86-64.
                let (iu, iv) = (fu as i32 as usize, fv as i32 as usize);
                let (au, av) = ((fu - iu as f64) as f32, (fv - iv as f64) as f32);
                // Bilinear weights of the four neighbours; clamped at the last row/column.
                let i00 = iv * w + iu;
                let i01 = if iu + 1 < w { i00 + 1 } else { i00 };
                let i10 = if iv + 1 < h { i00 + w } else { i00 };
                let i11 = i10 + (i01 - i00);
                let k = [(1.0 - au) * (1.0 - av), au * (1.0 - av), (1.0 - au) * av, au * av];
                let wpx = weights[i00] * k[0] + weights[i01] * k[1] + weights[i10] * k[2] + weights[i11] * k[3];
                if wpx <= 1e-4 {
                    continue;
                }
                let total = px.w + wpx;
                let mix = wpx / total;
                for ch in 0..3 {
                    let new = raw[i00 * 3 + ch] as f32 * k[0]
                        + raw[i01 * 3 + ch] as f32 * k[1]
                        + raw[i10 * 3 + ch] as f32 * k[2]
                        + raw[i11 * 3 + ch] as f32 * k[3];
                    let old = px.rgb[ch] as f32;
                    px.rgb[ch] = to_u8(old + (new - old) * mix);
                }
                px.luma = to_u8(luma(px.rgb));
                px.w = total.min(self.max_weight);
                touched = true;
            }
        }
        touched
    }
}

pub struct Canvas {
    tiles: TileGrid,
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
        Self { tiles: TileGrid::default(), bounds: None, max_weight: 4.0 }
    }

    pub fn clear(&mut self) {
        self.tiles = TileGrid::default();
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
        let t = self.tiles.get(tx, ty)?;
        let (lx, ly) = (x.rem_euclid(TILE), y.rem_euclid(TILE));
        let p = &t.px[(ly * TILE + lx) as usize];
        if p.w > 0.0 { Some(p) } else { None }
    }

    /// Bilinear luminance lookup; `None` where less than half of the neighbourhood is covered.
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
                acc += p.luma as f32 * w;
                wsum += w;
            }
        }
        if wsum > 0.5 { Some(acc / wsum) } else { None }
    }

    /// Blends `img` at `pose`. `weights` holds one weight per frame pixel.
    pub fn blend(&mut self, img: &RgbImage, weights: &[f32], pose: Pose) {
        let (w, h) = (img.width() as f64, img.height() as f64);
        let corners = [(-w, -h), (w, -h), (-w, h), (w, h)].map(|(u, v)| pose.apply(u / 2.0, v / 2.0));
        let x0 = corners.iter().map(|c| c.0).fold(f64::MAX, f64::min).floor() as i32;
        let x1 = corners.iter().map(|c| c.0).fold(f64::MIN, f64::max).ceil() as i32 + 1;
        let y0 = corners.iter().map(|c| c.1).fold(f64::MAX, f64::min).floor() as i32;
        let y1 = corners.iter().map(|c| c.1).fold(f64::MIN, f64::max).ceil() as i32 + 1;
        let area = Bounds { x0, y0, x1, y1 };
        let (tx0, tx1) = (x0.div_euclid(TILE), (x1 - 1).div_euclid(TILE));
        let (ty0, ty1) = (y0.div_euclid(TILE), (y1 - 1).div_euclid(TILE));
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                self.tiles.get_or_insert(tx, ty);
            }
        }
        // Tiles are independent: blend them in parallel.
        let job = BlendJob { img, weights, pose, area, max_weight: self.max_weight };
        let grid = &mut self.tiles;
        let (cols, gx0, gy0) = (grid.cols, grid.tx0, grid.ty0);
        let touched = grid
            .tiles
            .par_iter_mut()
            .enumerate()
            .filter_map(|(i, t)| {
                let (tx, ty) = (i as i32 % cols + gx0, i as i32 / cols + gy0);
                let in_range = (tx0..=tx1).contains(&tx) && (ty0..=ty1).contains(&ty);
                t.as_mut().filter(|_| in_range).map(|t| job.run(t, tx, ty))
            })
            .reduce(|| false, |a, b| a | b);
        if touched {
            self.bounds = Some(match self.bounds {
                None => area,
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
        let (sn, cs) = pose.theta.sin_cos();
        let st = step as f64;
        // Rows in parallel; each returns its number of covered samples and their sum.
        let (n, sum) =
            g.px.par_chunks_exact_mut(gw)
                .zip(mask.par_chunks_exact_mut(gw))
                .enumerate()
                .map(|(y, (row, mrow))| {
                    // Canvas position of sample (0, y), then one step along the frame's x axis.
                    let (px, py) = pose.apply(half - cx, y as f64 * st + half - cy);
                    let (mut px, mut py) = (px - 0.5, py - 0.5);
                    let (mut n, mut sum) = (0usize, 0.0f32);
                    for (v, m) in row.iter_mut().zip(mrow) {
                        if let Some(l) = self.sample_luma(px, py) {
                            *v = l;
                            *m = true;
                            n += 1;
                            sum += l;
                        }
                        px += cs * st;
                        py += sn * st;
                    }
                    (n, sum)
                })
                .reduce(|| (0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1));
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
        // Called for the live preview while the stitcher is locked: rows in parallel.
        out.par_chunks_exact_mut(ow as usize * 4).enumerate().for_each(|(oy, row)| {
            let y = b.y0 + ((oy as f32 + 0.5) * inv) as i32;
            for (ox, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let x = b.x0 + ((ox as f32 + 0.5) * inv) as i32;
                if let Some(p) = self.get(x, y) {
                    *px = [p.rgb[0], p.rgb[1], p.rgb[2], 255];
                }
            }
        });
        Some(out)
    }

    /// Full-resolution RGB export with the given background colour.
    pub fn render_rgb(&self, background: [u8; 3]) -> Option<RgbImage> {
        let b = self.bounds?;
        let mut out = RgbImage::from_pixel(b.width(), b.height(), Rgb(background));
        for ((tx, ty), tile) in self.tiles.iter() {
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
        for ((tx, ty), tile) in self.tiles.iter() {
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

/// Rounds and saturates to `u8`. Cheaper than `round().clamp()`, which calls
/// into libm when SSE4.1 is not enabled (the default x86-64 target).
#[inline]
pub fn to_u8(v: f32) -> u8 {
    // `as` saturates (NaN gives 0).
    (v + 0.5) as u8
}

#[inline]
pub fn luma(rgb: [u8; 3]) -> f32 {
    0.299 * rgb[0] as f32 + 0.587 * rgb[1] as f32 + 0.114 * rgb[2] as f32
}
