//! Real-time mosaicking of the frames captured while the mouse moves over a
//! document.
//!
//! Each frame is registered against the mosaic built so far (phase
//! correlation on two half-images gives translation and rotation), with a
//! constant-velocity / optical-sensor prediction used as a starting point and
//! as a fallback on featureless areas.  Registered frames are blended into a
//! tiled canvas with centre-weighted feathering.

pub mod canvas;
pub mod register;

use image::RgbImage;
use serde::{Deserialize, Serialize};

pub use canvas::{Bounds, Canvas, Pose};
use register::{Gray, PhaseCorrelator, Shift};

use crate::device::{Frame, MouseData};

/// Downsampling factor used for registration.
const STEP: u32 = 2;
/// Phase-correlation confidence considered reliable.
const MIN_CONFIDENCE: f32 = 7.0;

/// Per-pixel flat-field correction (vignetting + colour cast).
#[derive(Clone, Serialize, Deserialize)]
pub struct FlatField {
    pub width: u32,
    pub height: u32,
    /// RGB gains, row-major.
    pub gains: Vec<[f32; 3]>,
}

impl FlatField {
    /// Builds a flat field from frames of a uniform white sheet.
    pub fn from_white_frames(frames: &[&RgbImage]) -> Option<Self> {
        let first = frames.first()?;
        let (w, h) = first.dimensions();
        let n = (w * h) as usize;
        let mut acc = vec![[0f32; 3]; n];
        for f in frames {
            if f.dimensions() != (w, h) {
                continue;
            }
            for (a, p) in acc.iter_mut().zip(f.pixels()) {
                for k in 0..3 {
                    a[k] += p.0[k] as f32;
                }
            }
        }
        let cnt = frames.len() as f32;
        for a in &mut acc {
            for v in a.iter_mut() {
                *v /= cnt;
            }
        }
        let smooth = box_blur3(&acc, w as usize, h as usize, 6);
        // Target: the brightest (98th percentile) response of each channel.
        let mut target = [0f32; 3];
        for (k, t) in target.iter_mut().enumerate() {
            let mut v: Vec<f32> = smooth.iter().map(|p| p[k]).collect();
            v.sort_by(|a, b| a.total_cmp(b));
            *t = v[(v.len() as f32 * 0.98) as usize].max(1.0);
        }
        let t = target.iter().copied().fold(0.0, f32::max);
        let gains = smooth.iter().map(|p| [0, 1, 2].map(|k| (t / p[k].max(4.0)).min(8.0) * 0.96)).collect();
        Some(Self { width: w, height: h, gains })
    }

    pub fn apply(&self, img: &RgbImage) -> RgbImage {
        let mut out = img.clone();
        if img.dimensions() != (self.width, self.height) {
            return out;
        }
        for (p, g) in out.pixels_mut().zip(&self.gains) {
            for k in 0..3 {
                p.0[k] = (p.0[k] as f32 * g[k]).round().clamp(0.0, 255.0) as u8;
            }
        }
        out
    }
}

fn box_blur3(src: &[[f32; 3]], w: usize, h: usize, r: usize) -> Vec<[f32; 3]> {
    let mut tmp = vec![[0f32; 3]; w * h];
    let mut out = vec![[0f32; 3]; w * h];
    for y in 0..h {
        for x in 0..w {
            let (a, b) = (x.saturating_sub(r), (x + r).min(w - 1));
            let mut s = [0f32; 3];
            for xx in a..=b {
                for k in 0..3 {
                    s[k] += src[y * w + xx][k];
                }
            }
            let n = (b - a + 1) as f32;
            tmp[y * w + x] = s.map(|v| v / n);
        }
    }
    for y in 0..h {
        for x in 0..w {
            let (a, b) = (y.saturating_sub(r), (y + r).min(h - 1));
            let mut s = [0f32; 3];
            for yy in a..=b {
                for k in 0..3 {
                    s[k] += tmp[yy * w + x][k];
                }
            }
            let n = (b - a + 1) as f32;
            out[y * w + x] = s.map(|v| v / n);
        }
    }
    out
}

/// Self-calibrating illumination model.
///
/// The frame is divided into cells; for each cell the brightest robust value
/// (90th percentile) seen so far is tracked — on a document that is the paper
/// white.  A smooth quadratic surface is fitted per channel to those maxima
/// and used as a flat field.
pub struct AutoFlat {
    gw: usize,
    gh: usize,
    cell: u32,
    maxv: Vec<[f32; 3]>,
    seen: Vec<bool>,
    coeffs: Option<[[f64; 6]; 3]>,
    peak: [f32; 3],
}

impl AutoFlat {
    pub fn new(w: u32, h: u32) -> Self {
        let cell = 16;
        let (gw, gh) = ((w / cell) as usize, (h / cell) as usize);
        Self { gw, gh, cell, maxv: vec![[0.0; 3]; gw * gh], seen: vec![false; gw * gh], coeffs: None, peak: [0.0; 3] }
    }

    fn basis(x: f64, y: f64) -> [f64; 6] {
        [1.0, x, y, x * x, y * y, x * y]
    }

    pub fn update(&mut self, img: &RgbImage, margin: u32) {
        let (w, h) = img.dimensions();
        let c = self.cell;
        let mut vals: Vec<[u8; 3]> = Vec::with_capacity((c * c) as usize);
        for gy in 0..self.gh {
            for gx in 0..self.gw {
                let (x0, y0) = (gx as u32 * c, gy as u32 * c);
                // Skip the dark border/corners of the window.
                let corner = (x0 < 32 || x0 + c > w - 32) && (y0 < 32 || y0 + c > h - 32);
                if x0 < margin || y0 < margin || x0 + c > w - margin || y0 + c > h - margin || corner {
                    continue;
                }
                vals.clear();
                for y in y0..y0 + c {
                    for x in x0..x0 + c {
                        vals.push(img.get_pixel(x, y).0);
                    }
                }
                let i = gy * self.gw + gx;
                for k in 0..3 {
                    let mut ch: Vec<u8> = vals.iter().map(|p| p[k]).collect();
                    let n = ch.len();
                    let (_, v, _) = ch.select_nth_unstable(n * 9 / 10);
                    let v = *v as f32;
                    // Slow decay lets the model adapt if exposure changes.
                    self.maxv[i][k] = (self.maxv[i][k] * 0.998).max(v);
                }
                self.seen[i] = true;
            }
        }
        self.fit();
    }

    fn fit(&mut self) {
        let mut coeffs = [[0f64; 6]; 3];
        let mut peak = [0f32; 3];
        for k in 0..3 {
            let mut ata = [[0f64; 6]; 6];
            let mut atb = [0f64; 6];
            let mut n = 0;
            for gy in 0..self.gh {
                for gx in 0..self.gw {
                    let i = gy * self.gw + gx;
                    if !self.seen[i] || self.maxv[i][k] < 8.0 {
                        continue;
                    }
                    let x = (gx as f64 + 0.5) / self.gw as f64 - 0.5;
                    let y = (gy as f64 + 0.5) / self.gh as f64 - 0.5;
                    let b = Self::basis(x, y);
                    let v = self.maxv[i][k] as f64;
                    for r in 0..6 {
                        atb[r] += b[r] * v;
                        for c in 0..6 {
                            ata[r][c] += b[r] * b[c];
                        }
                    }
                    n += 1;
                    peak[k] = peak[k].max(self.maxv[i][k]);
                }
            }
            if n < 12 {
                self.coeffs = None;
                return;
            }
            for (r, row) in ata.iter_mut().enumerate() {
                row[r] += 1e-6;
            }
            match solve6(ata, atb) {
                Some(c) => coeffs[k] = c,
                None => {
                    self.coeffs = None;
                    return;
                }
            }
        }
        self.coeffs = Some(coeffs);
        self.peak = peak;
    }

    pub fn apply(&self, img: &RgbImage) -> RgbImage {
        let Some(coeffs) = self.coeffs else { return img.clone() };
        let (w, h) = img.dimensions();
        let target = self.peak.iter().copied().fold(0.0f32, f32::max) as f64;
        let mut out = img.clone();
        for y in 0..h {
            let fy = (y as f64 + 0.5) / h as f64 - 0.5;
            for x in 0..w {
                let fx = (x as f64 + 0.5) / w as f64 - 0.5;
                let b = Self::basis(fx, fy);
                let p = out.get_pixel_mut(x, y);
                for k in 0..3 {
                    let model: f64 = coeffs[k].iter().zip(&b).map(|(c, v)| c * v).sum();
                    let gain = (target / model.max(8.0)).clamp(0.5, 3.0) * 0.97;
                    p.0[k] = (p.0[k] as f64 * gain).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        out
    }
}

/// Gaussian elimination with partial pivoting for a 6x6 system.
fn solve6(mut a: [[f64; 6]; 6], mut b: [f64; 6]) -> Option<[f64; 6]> {
    for col in 0..6 {
        let piv = (col..6).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[piv][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        for r in col + 1..6 {
            let f = a[r][col] / a[col][col];
            for c in col..6 {
                a[r][c] -= f * a[col][c];
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = [0f64; 6];
    for r in (0..6).rev() {
        let s: f64 = (r + 1..6).map(|c| a[r][c] * x[c]).sum();
        x[r] = (b[r] - s) / a[r][r];
    }
    Some(x)
}

/// Tunable parameters of the stitcher.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StitchSettings {
    /// The camera sees the page through a mirror: flip frames before stitching.
    pub mirror: bool,
    /// Pixels ignored on each side of the frame.
    pub margin: u32,
    /// Width of the feathering ramp in pixels.
    pub feather: f32,
    /// Use the optical-sensor displacement as motion prior.
    pub use_sensor: bool,
    /// Optical-sensor counts to image pixels (x, y) — see [`SensorModel`].
    pub sensor_scale: f32,
    /// Estimate vignetting/illumination from the scanned paper when no
    /// calibration is available.
    #[serde(default = "default_true")]
    pub auto_flat: bool,
    /// Lift-off detection threshold on the optical sensors' surface quality
    /// (same meaning as the original `liftoffSensitivityLaser`, default 50).
    /// 0 disables the sensor-based detection (image brightness is still used).
    pub lift_sensitivity: u8,
    /// Quarter turns clockwise applied to the result (3 = 90° to the left).
    pub output_quarter_turns: u8,
}

fn default_true() -> bool {
    true
}

impl Default for StitchSettings {
    fn default() -> Self {
        Self {
            mirror: true,
            margin: 6,
            feather: 48.0,
            use_sensor: false,
            sensor_scale: 1.0,
            auto_flat: true,
            lift_sensitivity: 50,
            output_quarter_turns: 3,
        }
    }
}

/// Converts the raw words reported by the firmware into an in-frame displacement.
///
/// The exact meaning of the optical-sensor words could only be partially
/// recovered; this model treats words 0/1 and 3/4 as the signed (dx, dy)
/// counts of the two laser sensors, averaged.
#[derive(Clone, Copy, Debug)]
pub struct SensorModel {
    pub scale: f32,
}

impl SensorModel {
    pub fn displacement(&self, d: &MouseData) -> Option<(f32, f32)> {
        let s = |v: u16| v as i16 as f32;
        let (ax, ay, bx, by) = (s(d.words[0]), s(d.words[1]), s(d.words[3]), s(d.words[4]));
        if d.surface_quality[0] < 8 && d.surface_quality[1] < 8 {
            return None;
        }
        Some((((ax + bx) * 0.5) * self.scale, ((ay + by) * 0.5) * self.scale))
    }
}

/// Outcome of processing one frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct StepInfo {
    pub pose: Pose,
    /// Registration confidence (0 when the pose was only predicted).
    pub confidence: f32,
    /// Motion in pixels since the previous frame.
    pub speed: f32,
    /// The motion is too fast for reliable stitching.
    pub too_fast: bool,
    /// Registration failed, the pose is extrapolated.
    pub lost: bool,
    /// The mouse is lifted off the page: nothing is added to the mosaic.
    pub lifted: bool,
    /// Looking for the current position on the mosaic (after a lift or a loss).
    pub searching: bool,
}

/// Lift-off detection from the optical sensors and the image brightness.
#[derive(Default)]
struct LiftDetector {
    reference: f32,
    lifted: bool,
    bad: u32,
    good: u32,
}

impl LiftDetector {
    fn update(&mut self, brightness: f32, quality: Option<[u8; 2]>, threshold: u8) -> bool {
        let sensor_low = threshold > 0 && quality.is_some_and(|q| q[0] < threshold && q[1] < threshold);
        let dark = self.reference > 0.0 && brightness < self.reference * 0.5;
        if sensor_low || dark {
            self.bad += 1;
            self.good = 0;
        } else {
            self.good += 1;
            self.bad = 0;
            if !self.lifted {
                self.reference =
                    if self.reference == 0.0 { brightness } else { self.reference * 0.95 + brightness * 0.05 };
            }
        }
        if !self.lifted && self.bad >= 2 {
            self.lifted = true;
        } else if self.lifted && self.good >= 3 {
            self.lifted = false;
        }
        self.lifted
    }
}

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

pub struct Stitcher {
    pub settings: StitchSettings,
    pub flat: Option<FlatField>,
    auto_flat: AutoFlat,
    canvas: Canvas,
    weights: Vec<f32>,
    weights_dims: (u32, u32),
    full: PhaseCorrelator,
    half: PhaseCorrelator,
    coarse: PhaseCorrelator,
    lift: LiftDetector,
    /// Set after a lift-off or a prolonged loss: the position must be found again.
    searching: bool,
    pose: Pose,
    velocity: Pose,
    last_blend: Option<Pose>,
    prev_gray: Option<Gray>,
    lost_frames: u32,
    frames: u64,
}

impl Stitcher {
    pub fn new(settings: StitchSettings) -> Self {
        let (w, h) = (640 / STEP as usize, 296 / STEP as usize);
        Self {
            settings,
            flat: None,
            auto_flat: AutoFlat::new(640, 296),
            canvas: Canvas::new(),
            weights: Vec::new(),
            weights_dims: (0, 0),
            full: PhaseCorrelator::new(w, h),
            half: PhaseCorrelator::new(w / 2, h),
            coarse: PhaseCorrelator::new(w / 2, h / 2),
            lift: LiftDetector::default(),
            searching: false,
            pose: Pose::default(),
            velocity: Pose::default(),
            last_blend: None,
            prev_gray: None,
            lost_frames: 0,
            frames: 0,
        }
    }

    pub fn reset(&mut self) {
        self.canvas.clear();
        self.lift = LiftDetector::default();
        self.searching = false;
        self.auto_flat = AutoFlat::new(640, 296);
        self.pose = Pose::default();
        self.velocity = Pose::default();
        self.last_blend = None;
        self.prev_gray = None;
        self.lost_frames = 0;
        self.frames = 0;
    }

    pub fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    pub fn frame_count(&self) -> u64 {
        self.frames
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn pose(&self) -> Pose {
        self.pose
    }

    fn ensure_weights(&mut self, w: u32, h: u32) {
        if self.weights_dims == (w, h) {
            return;
        }
        let m = self.settings.margin as f32;
        let f = self.settings.feather.max(1.0);
        let corner = 28.0f32;
        let mut wt = vec![0f32; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let dx = (x as f32 + 0.5).min(w as f32 - x as f32 - 0.5) - m;
                let dy = (y as f32 + 0.5).min(h as f32 - y as f32 - 0.5) - m;
                if dx <= 0.0 || dy <= 0.0 {
                    continue;
                }
                // Rounded, dark corners of the scan window.
                if dx < corner && dy < corner {
                    let r = ((corner - dx).powi(2) + (corner - dy).powi(2)).sqrt();
                    if r > corner {
                        continue;
                    }
                }
                let e = (dx.min(dy) / f).min(1.0);
                wt[(y * w + x) as usize] = e * e * (3.0 - 2.0 * e) + 1e-3;
            }
        }
        self.weights = wt;
        self.weights_dims = (w, h);
    }

    /// Applies orientation and flat-field correction.
    pub fn prepare(&mut self, img: &RgbImage) -> RgbImage {
        let mut out = match &self.flat {
            Some(f) => f.apply(img),
            None if self.settings.auto_flat && img.dimensions() == (640, 296) => {
                self.auto_flat.update(img, self.settings.margin);
                self.auto_flat.apply(img)
            }
            None => img.clone(),
        };
        if self.settings.mirror {
            // A vertical flip (= horizontal mirror + half turn) gives upright
            // text when the mouse is held normally on the page.
            image::imageops::flip_vertical_in_place(&mut out);
        }
        out
    }

    fn to_gray(&self, img: &RgbImage) -> Gray {
        let (w, h) = ((img.width() / STEP) as usize, (img.height() / STEP) as usize);
        let mut g = Gray::new(w, h);
        let s = STEP as usize;
        let raw = img.as_raw();
        let iw = img.width() as usize;
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0;
                for yy in 0..s {
                    for xx in 0..s {
                        let i = ((y * s + yy) * iw + x * s + xx) * 3;
                        acc += canvas::luma([raw[i], raw[i + 1], raw[i + 2]]);
                    }
                }
                g.px[y * w + x] = acc / (s * s) as f32;
            }
        }
        g
    }

    /// Estimates (du, dv, dtheta) such that `cur(p) ≈ reference(p + d + dtheta·J p)`.
    fn estimate(&mut self, reference: &Gray, cur: &Gray) -> Option<(f64, f64, f64, f32)> {
        let full: Shift = self.full.correlate(reference, cur);
        if full.confidence < MIN_CONFIDENCE {
            return None;
        }
        let hw = reference.w / 2;
        let l = self.half.correlate(&reference.crop(0, 0, hw, reference.h), &cur.crop(0, 0, hw, cur.h));
        let r = self.half.correlate(&reference.crop(hw, 0, hw, reference.h), &cur.crop(hw, 0, hw, cur.h));
        let s = STEP as f64;
        let mut dtheta = 0.0;
        let (mut du, mut dv) = (full.dx as f64 * s, full.dy as f64 * s);
        let consistent = |h: &Shift| {
            h.confidence >= MIN_CONFIDENCE * 0.8 && (h.dx - full.dx).abs() < 6.0 && (h.dy - full.dy).abs() < 6.0
        };
        if consistent(&l) && consistent(&r) {
            // Half-image centres are `hw` (downsampled) pixels apart.
            dtheta = ((r.dy - l.dy) as f64 * s / (hw as f64 * s)).atan();
            du = (l.dx + r.dx) as f64 * 0.5 * s;
            dv = (l.dy + r.dy) as f64 * 0.5 * s;
        }
        Some((du, dv, dtheta, full.confidence))
    }

    /// Adds a frame to the mosaic.
    pub fn push(&mut self, frame: &Frame) -> StepInfo {
        let brightness = mean_brightness(&frame.image);
        let img = self.prepare(&frame.image);
        self.push_measured(&img, Some(&frame.data), brightness)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn push_image(&mut self, img: &RgbImage, data: Option<&MouseData>) -> StepInfo {
        let brightness = mean_brightness(img);
        self.push_measured(img, data, brightness)
    }

    /// Searches the position of the frame on the mosaic around the last trusted pose
    /// (after the mouse was put down again or registration was lost).
    fn relocalize(&mut self, gray: &Gray, w: u32, h: u32) -> Option<(Pose, f32)> {
        let small = downsample2(gray);
        let anchor = self.pose;
        let mut best: Option<(Pose, f32)> = None;
        let (ox, oy) = (w as f64 / 3.0, h as f64 / 3.0);
        for dt_deg in [0.0f64, -6.0, 6.0, -12.0, 12.0, -18.0, 18.0, -24.0, 24.0, -30.0, 30.0] {
            for (gx, gy) in [
                (0.0, 0.0),
                (1.0, 0.0),
                (-1.0, 0.0),
                (0.0, 1.0),
                (0.0, -1.0),
                (1.0, 1.0),
                (-1.0, 1.0),
                (1.0, -1.0),
                (-1.0, -1.0),
            ] {
                let cand = anchor.moved_local(gx * ox, gy * oy, dt_deg.to_radians());
                let (reference, coverage) = self.canvas.sample_gray(cand, w, h, STEP * 2);
                if coverage < 0.3 {
                    continue;
                }
                let s = self.coarse.correlate(&reference, &small);
                if best.as_ref().is_none_or(|b| s.confidence > b.1) {
                    let f = (STEP * 2) as f64;
                    best = Some((cand.moved_local(s.dx as f64 * f, s.dy as f64 * f, 0.0), s.confidence));
                }
            }
        }
        let (mut pose, conf) = best?;
        if conf < MIN_CONFIDENCE * 1.3 {
            return None;
        }
        // Refine at full registration resolution and require a confident match.
        let mut last_conf = 0.0;
        for _ in 0..2 {
            let (reference, coverage) = self.canvas.sample_gray(pose, w, h, STEP);
            if coverage < 0.35 {
                return None;
            }
            let (du, dv, dt, c) = self.estimate(&reference, gray)?;
            pose = pose.moved_local(du, dv, dt);
            last_conf = c;
        }
        (last_conf >= MIN_CONFIDENCE * 1.2).then_some((pose, last_conf))
    }

    fn push_measured(&mut self, img: &RgbImage, data: Option<&MouseData>, brightness: f32) -> StepInfo {
        let (w, h) = img.dimensions();
        self.ensure_weights(w, h);
        let gray = self.to_gray(img);
        self.frames += 1;

        let lifted = self.lift.update(brightness, data.map(|d| d.surface_quality), self.settings.lift_sensitivity);
        if lifted {
            // Nothing usable while the mouse is in the air; remember to search on landing.
            if !self.canvas.is_empty() {
                self.searching = true;
            }
            self.velocity = Pose::default();
            self.prev_gray = None;
            return StepInfo {
                pose: self.pose,
                lifted: true,
                searching: self.searching,
                lost: true,
                ..Default::default()
            };
        }
        if self.searching {
            match self.relocalize(&gray, w, h) {
                Some((pose, conf)) => {
                    self.searching = false;
                    self.lost_frames = 0;
                    self.pose = pose;
                    self.velocity = Pose::default();
                    self.prev_gray = Some(gray);
                    return StepInfo { pose, confidence: conf, ..Default::default() };
                }
                None => {
                    return StepInfo { pose: self.pose, searching: true, lost: true, ..Default::default() };
                }
            }
        }

        if self.canvas.is_empty() {
            self.pose = Pose::default();
            let weights = std::mem::take(&mut self.weights);
            self.canvas.blend(img, &weights, self.pose);
            self.weights = weights;
            self.last_blend = Some(self.pose);
            self.prev_gray = Some(gray);
            return StepInfo { pose: self.pose, confidence: 1.0, ..Default::default() };
        }

        // Prediction: optical sensor if enabled, otherwise constant velocity.
        let sensor = if self.settings.use_sensor {
            data.and_then(|d| SensorModel { scale: self.settings.sensor_scale }.displacement(d))
        } else {
            None
        };
        let predicted = match sensor {
            Some((dx, dy)) => self.pose.moved_local(dx as f64, dy as f64, self.velocity.theta),
            None => self.pose.moved_local(self.velocity.x, self.velocity.y, self.velocity.theta),
        };

        let mut result: Option<(Pose, f32)> = None;
        // 1. Register against the mosaic (limits drift, closes small loops).
        let (reference, coverage) = self.canvas.sample_gray(predicted, w, h, STEP);
        if coverage > 0.35
            && let Some((du, dv, dt, conf)) = self.estimate(&reference, &gray)
        {
            let mut pose = predicted.moved_local(du, dv, dt);
            // Refinement pass at the corrected pose.
            let (reference2, cov2) = self.canvas.sample_gray(pose, w, h, STEP);
            if cov2 > 0.35
                && let Some((du2, dv2, dt2, conf2)) = self.estimate(&reference2, &gray)
                && du2.abs() < 8.0
                && dv2.abs() < 8.0
            {
                pose = pose.moved_local(du2, dv2, dt2);
                result = Some((pose, conf2));
            }
            if result.is_none() {
                result = Some((pose, conf));
            }
        }
        // 2. Fall back to frame-to-frame registration.
        if result.is_none()
            && let Some(prev) = self.prev_gray.take()
        {
            if let Some((du, dv, dt, conf)) = self.estimate(&prev, &gray) {
                result = Some((self.pose.moved_local(du, dv, dt), conf));
            }
            self.prev_gray = Some(prev);
        }

        let (pose, confidence, lost) = match result {
            Some((p, c)) => (p, c, false),
            None => (predicted, 0.0, true),
        };
        // Velocity in the previous frame's axes.
        let (lu, lv) = self.pose.inverse(pose.x, pose.y);
        let speed = (lu * lu + lv * lv).sqrt() as f32;
        let too_fast = speed > h as f32 * 0.45;
        if !lost {
            let a = 0.6;
            self.velocity = Pose::new(
                self.velocity.x * (1.0 - a) + lu * a,
                self.velocity.y * (1.0 - a) + lv * a,
                self.velocity.theta * (1.0 - a) + (pose.theta - self.pose.theta) * a,
            );
            self.lost_frames = 0;
        } else {
            self.lost_frames += 1;
            if self.lost_frames > 4 {
                // Stop extrapolating: wait until the frame can be located on the mosaic.
                self.searching = true;
                self.velocity = Pose::default();
                return StepInfo { pose: self.pose, searching: true, lost: true, speed, ..Default::default() };
            }
            // Decay the extrapolation so a stationary mouse does not drift away.
            self.velocity = Pose::new(self.velocity.x * 0.5, self.velocity.y * 0.5, self.velocity.theta * 0.5);
        }
        self.pose = pose;
        self.prev_gray = Some(gray);

        // Blend when the frame brings new content (and the pose is trusted).
        let moved = self.last_blend.is_none_or(|b| {
            let (dx, dy) = (pose.x - b.x, pose.y - b.y);
            (dx * dx + dy * dy).sqrt() > 3.0 || (pose.theta - b.theta).abs() > 0.004
        });
        if moved && !lost && !too_fast {
            let weights = std::mem::take(&mut self.weights);
            self.canvas.blend(img, &weights, pose);
            self.weights = weights;
            self.last_blend = Some(pose);
        }
        StepInfo { pose, confidence, speed, too_fast, lost, ..Default::default() }
    }

    /// Final image: cropped to the scanned area, with automatic white balance.
    pub fn finish(&self, auto_levels: bool) -> Option<RgbImage> {
        let mut img = self.canvas.render_rgb([255, 255, 255])?;
        if auto_levels && let Some((_, mask)) = self.canvas.coverage() {
            auto_white_balance(&mut img, &mask);
        }
        Some(orient_image(self.settings.output_quarter_turns, &img))
    }
}

/// Mean luminance of a subsample of the frame (0..255).
pub fn mean_brightness(img: &RgbImage) -> f32 {
    let (w, h) = img.dimensions();
    let mut acc = 0.0;
    let mut n = 0u32;
    for y in (h / 8..h - h / 8).step_by(8) {
        for x in (w / 8..w - w / 8).step_by(8) {
            acc += canvas::luma(img.get_pixel(x, y).0);
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { acc / n as f32 }
}

fn downsample2(g: &Gray) -> Gray {
    let (w, h) = (g.w / 2, g.h / 2);
    let mut out = Gray::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let i = 2 * y * g.w + 2 * x;
            out.px[y * w + x] = (g.px[i] + g.px[i + 1] + g.px[i + g.w] + g.px[i + g.w + 1]) * 0.25;
        }
    }
    out
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

    /// Synthetic "document": pseudo-random blobs on white.
    fn document(w: u32, h: u32) -> RgbImage {
        let mut img = RgbImage::from_pixel(w, h, Rgb([240, 240, 235]));
        let mut seed = 12345u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..1500 {
            let cx = (rnd() % w as u64) as i32;
            let cy = (rnd() % h as u64) as i32;
            let r = (rnd() % 9 + 2) as i32;
            let c = (rnd() % 120) as u8;
            for y in (cy - r).max(0)..(cy + r).min(h as i32) {
                for x in (cx - r).max(0)..(cx + r).min(w as i32) {
                    if (x - cx).pow(2) + (y - cy).pow(2) <= r * r {
                        img.put_pixel(x as u32, y as u32, Rgb([c, c / 2, c]));
                    }
                }
            }
        }
        img
    }

    fn view(doc: &RgbImage, pose: Pose) -> RgbImage {
        let mut out = RgbImage::new(640, 296);
        for v in 0..296 {
            for u in 0..640 {
                let (x, y) = pose.apply(u as f64 + 0.5 - 320.0, v as f64 + 0.5 - 148.0);
                let (xi, yi) = (x.round() as i32, y.round() as i32);
                if xi >= 0 && yi >= 0 && (xi as u32) < doc.width() && (yi as u32) < doc.height() {
                    out.put_pixel(u, v, *doc.get_pixel(xi as u32, yi as u32));
                }
            }
        }
        out
    }

    fn mouse(q: u8) -> MouseData {
        MouseData { surface_quality: [q, q], ..Default::default() }
    }

    #[test]
    fn lift_and_relocalize() {
        let doc = document(2000, 1400);
        let mut st = Stitcher::new(StitchSettings::default());
        let start = Pose::new(500.0, 400.0, 0.0);
        let mut truth = start;
        let on = mouse(140);
        for _ in 0..40 {
            let info = st.push_image(&view(&doc, truth), Some(&on));
            assert!(!info.lost && !info.lifted);
            truth = truth.moved_local(12.0, 3.0, 0.0);
        }
        let before = st.pose();
        // Lift: the camera sees a dark, blurry image and the sensors lose the surface.
        let dark = RgbImage::from_pixel(640, 296, Rgb([20, 25, 15]));
        let mut lifted_seen = false;
        for _ in 0..10 {
            let info = st.push_image(&dark, Some(&mouse(5)));
            lifted_seen |= info.lifted;
        }
        assert!(lifted_seen, "lift-off not detected");
        // Put the mouse down elsewhere on the scanned area, slightly rotated.
        let landing = truth.moved_local(-150.0, 60.0, 0.15);
        let mut found = None;
        for i in 0..8 {
            let info = st.push_image(&view(&doc, landing), Some(&on));
            if !info.lifted && !info.searching {
                found = Some((i, info.pose));
                break;
            }
        }
        let (_, pose) = found.expect("position not found after landing");
        // Compare relative to the pose before the lift (canvas origin = first frame).
        let expected_dx = landing.x - start.x;
        let expected_dy = landing.y - start.y;
        assert!(
            (pose.x - expected_dx).abs() < 6.0 && (pose.y - expected_dy).abs() < 6.0,
            "pose {pose:?} expected ({expected_dx}, {expected_dy}), before lift {before:?}"
        );
        assert!((pose.theta - landing.theta).abs() < 0.02, "theta {}", pose.theta);
    }

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

    #[test]
    fn tracks_synthetic_motion() {
        let doc = document(2000, 1400);
        let mut st = Stitcher::new(StitchSettings::default());
        let start = Pose::new(500.0, 400.0, 0.0);
        let mut truth = start;
        let mut last = StepInfo::default();
        for i in 0..60 {
            let img = view(&doc, truth);
            last = st.push_image(&img, None);
            assert!(!last.lost, "lost at frame {i}");
            truth = truth.moved_local(14.0, 5.0, 0.004);
        }
        // Compare the relative motion from the first frame.
        let est = st.pose();
        let truth_last = Pose { x: truth.x, y: truth.y, theta: truth.theta };
        let _ = truth_last;
        let expected = {
            let mut p = start;
            for _ in 0..59 {
                p = p.moved_local(14.0, 5.0, 0.004);
            }
            p
        };
        let ex = expected.x - start.x;
        let ey = expected.y - start.y;
        assert!(
            (est.x - ex).abs() < 6.0 && (est.y - ey).abs() < 6.0,
            "est {est:?} expected ({ex}, {ey}) last {last:?}"
        );
        assert!((est.theta - expected.theta).abs() < 0.02, "theta {} vs {}", est.theta, expected.theta);
    }
}
