//! Frame registration and mosaicking.

use image::RgbImage;
use rayon::prelude::*;

use super::StitchSettings;
use super::canvas::{self, Canvas, Pose};
use super::flat::{AutoFlat, FlatField};
use super::laser::LaserOdometry;
use super::lift::LiftDetector;
use super::output::{auto_white_balance, orient_image};
use super::register::{Gray, PhaseCorrelator, Shift};
use crate::device::{FRAME_HEIGHT, FRAME_WIDTH, Frame, MouseData};

/// The camera sees the page at an angle (through a mirror): a frame pixel
/// covers more paper along v than along u. Measured with the laser sensors,
/// whose counts per pixel differ by ~8.4 % between the two axes — the same
/// for both sensors. Frames are stretched along v to square pixels before
/// stitching; otherwise rotating the mouse is not a rigid motion of the image.
/// The measured ratio is 1.084; 320 rows (1.081) keep the FFT sizes small.
pub const STITCH_FRAME: (u32, u32) = (FRAME_WIDTH, 320);
/// Downsampling factor used for registration.
const STEP: u32 = 2;
/// Centred part of the frame used for registration. Its dimensions are rounded
/// down to multiples of 16 so that every FFT size (full, half and coarse images)
/// only has small prime factors (148 = 4 × 37 rows, for instance, makes the FFT
/// several times slower).
const REG_W: u32 = STITCH_FRAME.0 & !15;
const REG_H: u32 = STITCH_FRAME.1 & !15;
// The registration window must be centred exactly on the frame centre.
const _: () = assert!((STITCH_FRAME.0 - REG_W).is_multiple_of(2) && (STITCH_FRAME.1 - REG_H).is_multiple_of(2));
/// Phase-correlation confidence considered reliable.
const MIN_CONFIDENCE: f32 = 7.0;

/// Largest disagreement tolerated between image registration and the laser
/// measurement (pixels, radians). Beyond it the image locked onto a wrong
/// match — typically one period away on ruled lines or table borders.
const LASER_TOLERANCE: (f64, f64) = (24.0, 0.04);

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
    /// The pose comes from the laser sensors only (blank area, or the image
    /// registration contradicted them).
    pub laser_only: bool,
    /// The mouse is lifted off the page: nothing is added to the mosaic.
    pub lifted: bool,
    /// Looking for the current position on the mosaic (after a lift or a loss).
    pub searching: bool,
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
    laser: LaserOdometry,
    /// Set after a lift-off or a prolonged loss: the position must be found again.
    searching: bool,
    pose: Pose,
    /// Smoothed translation per frame, in the frame's axes. Rotation is never
    /// extrapolated: when it cannot be measured (only one half of the frame has
    /// texture) a predicted rotation would feed back into itself and drift.
    velocity: (f64, f64),
    last_blend: Option<Pose>,
    prev_gray: Option<Gray>,
    lost_frames: u32,
    frames: u64,
}

impl Stitcher {
    pub fn new(settings: StitchSettings) -> Self {
        let (w, h) = ((REG_W / STEP) as usize, (REG_H / STEP) as usize);
        Self {
            settings,
            flat: None,
            auto_flat: AutoFlat::new(FRAME_WIDTH, FRAME_HEIGHT),
            canvas: Canvas::new(),
            weights: Vec::new(),
            weights_dims: (0, 0),
            full: PhaseCorrelator::new(w, h),
            half: PhaseCorrelator::new(w / 2, h),
            coarse: PhaseCorrelator::new(w / 2, h / 2),
            lift: LiftDetector::default(),
            laser: LaserOdometry::default(),
            searching: false,
            pose: Pose::default(),
            velocity: (0.0, 0.0),
            last_blend: None,
            prev_gray: None,
            lost_frames: 0,
            frames: 0,
        }
    }

    pub fn reset(&mut self) {
        self.canvas.clear();
        self.lift = LiftDetector::default();
        self.laser.reset();
        self.searching = false;
        self.auto_flat = AutoFlat::new(FRAME_WIDTH, FRAME_HEIGHT);
        self.pose = Pose::default();
        self.velocity = (0.0, 0.0);
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
            None if self.settings.auto_flat && img.dimensions() == (FRAME_WIDTH, FRAME_HEIGHT) => {
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
        stretch_rows(&out, STITCH_FRAME.1)
    }

    /// Downsampled luminance of the registration window (centre of the frame).
    fn to_gray(img: &RgbImage) -> Gray {
        let (w, h) = ((REG_W / STEP) as usize, (REG_H / STEP) as usize);
        let mut g = Gray::new(w, h);
        let s = STEP as usize;
        let raw = img.as_raw();
        let iw = img.width() as usize;
        let x0 = (img.width().saturating_sub(REG_W) / 2) as usize;
        let y0 = (img.height().saturating_sub(REG_H) / 2) as usize;
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0;
                for yy in 0..s {
                    for xx in 0..s {
                        let i = ((y0 + y * s + yy) * iw + x0 + x * s + xx) * 3;
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
        let half = &self.half;
        let (l, r) = rayon::join(
            || half.correlate(&reference.crop(0, 0, hw, reference.h), &cur.crop(0, 0, hw, cur.h)),
            || half.correlate(&reference.crop(hw, 0, hw, reference.h), &cur.crop(hw, 0, hw, cur.h)),
        );
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
        let (ox, oy) = (w as f64 / 3.0, h as f64 / 3.0);
        const ANGLES: [f64; 11] = [0.0, -6.0, 6.0, -12.0, 12.0, -18.0, 18.0, -24.0, 24.0, -30.0, 30.0];
        const OFFSETS: [(f64, f64); 9] = [
            (0.0, 0.0),
            (1.0, 0.0),
            (-1.0, 0.0),
            (0.0, 1.0),
            (0.0, -1.0),
            (1.0, 1.0),
            (-1.0, 1.0),
            (1.0, -1.0),
            (-1.0, -1.0),
        ];
        let (canvas, coarse) = (&self.canvas, &self.coarse);
        let f = (STEP * 2) as f64;
        // Candidates are independent: try them in parallel. On equal confidence
        // the first one (smallest rotation and offset) wins, as in a sequential search.
        let best = (0..ANGLES.len() * OFFSETS.len())
            .into_par_iter()
            .filter_map(|i| {
                let (gx, gy) = OFFSETS[i % OFFSETS.len()];
                let cand = anchor.moved_local(gx * ox, gy * oy, ANGLES[i / OFFSETS.len()].to_radians());
                let (reference, coverage) = canvas.sample_gray(cand, REG_W, REG_H, STEP * 2);
                if coverage < 0.3 {
                    return None;
                }
                let s = coarse.correlate(&reference, &small);
                Some((i, cand.moved_local(s.dx as f64 * f, s.dy as f64 * f, 0.0), s.confidence))
            })
            .max_by(|a, b| a.2.total_cmp(&b.2).then(b.0.cmp(&a.0)))
            .map(|(_, pose, conf)| (pose, conf));
        let (mut pose, conf) = best?;
        if conf < MIN_CONFIDENCE * 1.3 {
            return None;
        }
        // Refine at full registration resolution and require a confident match.
        let mut last_conf = 0.0;
        for _ in 0..2 {
            let (reference, coverage) = self.canvas.sample_gray(pose, REG_W, REG_H, STEP);
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
        let gray = Self::to_gray(img);
        self.frames += 1;

        let laser = match data {
            Some(d) if self.settings.use_laser => self.laser.update(d, self.settings.mirror),
            _ => None,
        };
        let lifted = self.lift.update(brightness, data.map(|d| d.surface_quality), self.settings.lift_sensitivity);
        if lifted {
            self.laser.reset();
            // Nothing usable while the mouse is in the air; remember to search on landing.
            if !self.canvas.is_empty() {
                self.searching = true;
            }
            self.velocity = (0.0, 0.0);
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
                    self.velocity = (0.0, 0.0);
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

        // Prediction: laser sensors when available, otherwise constant velocity.
        let predicted = match laser {
            Some((du, dv, dtheta)) => self.pose.moved_local(du, dv, dtheta),
            None => self.pose.moved_local(self.velocity.0, self.velocity.1, 0.0),
        };

        let mut result: Option<(Pose, f32)> = None;
        // 1. Register against the mosaic (limits drift, closes small loops).
        let (reference, coverage) = self.canvas.sample_gray(predicted, REG_W, REG_H, STEP);
        if coverage > 0.35
            && let Some((du, dv, dt, conf)) = self.estimate(&reference, &gray)
        {
            let mut pose = predicted.moved_local(du, dv, dt);
            // Refinement pass at the corrected pose.
            let (reference2, cov2) = self.canvas.sample_gray(pose, REG_W, REG_H, STEP);
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

        // The laser is a measurement too: an image match far from it is wrong.
        if laser.is_some()
            && let Some((p, _)) = result
        {
            let (du, dv) = predicted.inverse(p.x, p.y);
            if (du * du + dv * dv).sqrt() > LASER_TOLERANCE.0 || (p.theta - predicted.theta).abs() > LASER_TOLERANCE.1 {
                result = None;
            }
        }
        let (pose, confidence, lost, laser_only) = match result {
            Some((p, c)) => (p, c, false, false),
            None if laser.is_some() => (predicted, 0.0, false, true),
            None => (predicted, 0.0, true, false),
        };
        // Velocity in the previous frame's axes.
        let (lu, lv) = self.pose.inverse(pose.x, pose.y);
        let speed = (lu * lu + lv * lv).sqrt() as f32;
        let too_fast = speed > h as f32 * 0.45;
        if !lost {
            let a = 0.6;
            self.velocity = (self.velocity.0 * (1.0 - a) + lu * a, self.velocity.1 * (1.0 - a) + lv * a);
            self.lost_frames = 0;
        } else {
            self.lost_frames += 1;
            if self.lost_frames > 4 {
                // Stop extrapolating: wait until the frame can be located on the mosaic.
                self.searching = true;
                self.velocity = (0.0, 0.0);
                return StepInfo { pose: self.pose, searching: true, lost: true, speed, ..Default::default() };
            }
            // Decay the extrapolation so a stationary mouse does not drift away.
            self.velocity = (self.velocity.0 * 0.5, self.velocity.1 * 0.5);
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
        StepInfo { pose, confidence, speed, too_fast, lost, laser_only, ..Default::default() }
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

/// Resamples `img` to `height` rows (linear interpolation between rows).
fn stretch_rows(img: &RgbImage, height: u32) -> RgbImage {
    let (w, h) = img.dimensions();
    if h == height {
        return img.clone();
    }
    let stride = w as usize * 3;
    let src = img.as_raw();
    let k = h as f32 / height as f32;
    let mut out = RgbImage::new(w, height);
    for (y, row) in out.chunks_exact_mut(stride).enumerate() {
        let sy = ((y as f32 + 0.5) * k - 0.5).clamp(0.0, (h - 1) as f32);
        let (y0, a) = (sy as usize, sy.fract());
        let y1 = (y0 + 1).min(h as usize - 1);
        let (r0, r1) = (&src[y0 * stride..][..stride], &src[y1 * stride..][..stride]);
        for ((o, &p0), &p1) in row.iter_mut().zip(r0).zip(r1) {
            *o = canvas::to_u8(p0 as f32 + (p1 as f32 - p0 as f32) * a);
        }
    }
    out
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
        let (w, h) = STITCH_FRAME;
        let mut out = RgbImage::new(w, h);
        for v in 0..h {
            for u in 0..w {
                let (x, y) = pose.apply(u as f64 + 0.5 - w as f64 / 2.0, v as f64 + 0.5 - h as f64 / 2.0);
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

    /// Moving along the edge of the text, with only one half of the frame
    /// textured, must not make the rotation drift.
    #[test]
    fn rotation_stays_put_along_blank_margin() {
        let mut doc = document(2000, 1400);
        for (x, _, p) in doc.enumerate_pixels_mut() {
            if x >= 900 {
                *p = Rgb([240, 240, 235]);
            }
        }
        let mut st = Stitcher::new(StitchSettings { auto_flat: false, ..Default::default() });
        let start = Pose { x: 700.0, y: 300.0, theta: 0.0 };
        let mut truth = start;
        // Turn slightly while moving towards the margin...
        for _ in 0..30 {
            st.push_image(&view(&doc, truth), None);
            truth = truth.moved_local(11.0, 0.0, 0.003);
        }
        // ...then go down the margin without turning. Only the drift during this
        // phase is checked: part of the turn above may be missed for the same reason.
        let error_before = st.pose().theta - (truth.theta - start.theta);
        for i in 0..60 {
            let info = st.push_image(&view(&doc, truth), None);
            assert!(!info.searching, "lost at frame {i}");
            let (s, c) = truth.theta.sin_cos();
            truth = Pose { x: truth.x - s * 10.0, y: truth.y + c * 10.0, ..truth };
        }
        let drift = st.pose().theta - (truth.theta - start.theta) - error_before;
        assert!(drift.abs() < 0.02, "rotation drifted by {drift:.3} rad");
    }

    #[test]
    fn lift_and_relocalize() {
        let doc = document(2000, 1400);
        let mut st = Stitcher::new(StitchSettings::default());
        let start = Pose { x: 500.0, y: 400.0, theta: 0.0 };
        let mut truth = start;
        let on = mouse(140);
        for _ in 0..40 {
            let info = st.push_image(&view(&doc, truth), Some(&on));
            assert!(!info.lost && !info.lifted);
            truth = truth.moved_local(12.0, 3.0, 0.0);
        }
        let before = st.pose();
        // Lift: the camera sees a dark, blurry image and the sensors lose the surface.
        let dark = RgbImage::from_pixel(STITCH_FRAME.0, STITCH_FRAME.1, Rgb([20, 25, 15]));
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
    fn tracks_synthetic_motion() {
        let doc = document(2000, 1400);
        let mut st = Stitcher::new(StitchSettings::default());
        let start = Pose { x: 500.0, y: 400.0, theta: 0.0 };
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
