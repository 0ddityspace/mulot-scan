//! Locating a frame on a mosaic: registration around a predicted pose, and
//! global search after a lift-off or a loss of tracking.

use rayon::prelude::*;

use super::canvas::{Canvas, Pose};
use super::register::{Gray, PhaseCorrelator, Shift};
use super::stitcher::{MIN_CONFIDENCE, REG_H, REG_W, STEP};

/// Normalised cross-correlation between a frame and the mosaic required to
/// accept a relocalization. Correctly placed frames score 0.9–0.97 on real
/// recordings, the best wrong candidates around 0.5.
pub(super) const MIN_SIMILARITY: f32 = 0.7;
/// Rotations tried by the global search, relative to the anchor (degrees).
const ANGLES: [f64; 11] = [0.0, -6.0, 6.0, -12.0, 12.0, -18.0, 18.0, -24.0, 24.0, -30.0, 30.0];

/// Phase correlators at the three resolutions used for registration.
pub(super) struct Matcher {
    full: PhaseCorrelator,
    half: PhaseCorrelator,
    coarse: PhaseCorrelator,
}

impl Matcher {
    pub(super) fn new() -> Self {
        let (w, h) = ((REG_W / STEP) as usize, (REG_H / STEP) as usize);
        Self {
            full: PhaseCorrelator::new(w, h),
            half: PhaseCorrelator::new(w / 2, h),
            coarse: PhaseCorrelator::new(w / 2, h / 2),
        }
    }

    /// Estimates (du, dv, dtheta) such that `cur(p) ≈ reference(p + d + dtheta·J p)`.
    pub(super) fn estimate(&self, reference: &Gray, cur: &Gray) -> Option<(f64, f64, f64, f32)> {
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

    /// Registers the frame on `canvas` starting from `pose` (two passes).
    /// Returns the refined pose and the registration confidence.
    pub(super) fn refine(&self, canvas: &Canvas, gray: &Gray, mut pose: Pose) -> Option<(Pose, f32)> {
        let mut conf = 0.0;
        for _ in 0..2 {
            let (reference, coverage) = canvas.sample_gray(pose, REG_W, REG_H, STEP);
            if coverage < 0.35 {
                return None;
            }
            let (du, dv, dt, c) = self.estimate(&reference, gray)?;
            pose = pose.moved_local(du, dv, dt);
            conf = c;
        }
        Some((pose, conf))
    }

    /// Looks for the frame anywhere on `canvas`. The thumbnails of `index`
    /// select a few candidates, which are then registered properly; a match is
    /// accepted only if the frame really looks like the mosaic there. Returns
    /// the pose, registration confidence and similarity of the best one.
    pub(super) fn search(&self, canvas: &Canvas, index: &mut SearchIndex, gray: &Gray) -> Option<(Pose, f32, f32)> {
        let candidates = index.best(canvas, gray, SHORTLIST);
        let small = downsample2(gray);
        let f = (STEP * 2) as f64;
        let coarse = &self.coarse;
        let mut hits: Vec<(usize, Pose, f32)> = candidates
            .par_iter()
            .enumerate()
            .filter_map(|(i, &cand)| {
                let (reference, coverage) = canvas.sample_gray(cand, REG_W, REG_H, STEP * 2);
                if coverage < 0.3 {
                    return None;
                }
                let s = coarse.correlate(&reference, &small);
                (s.confidence >= MIN_CONFIDENCE)
                    .then(|| (i, cand.moved_local(s.dx as f64 * f, s.dy as f64 * f, 0.0), s.confidence))
            })
            .collect();
        // Best coarse matches first; on equal confidence the earlier candidate wins.
        hits.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
        hits.truncate(3);
        hits.par_iter()
            .filter_map(|&(_, pose, _)| {
                let (pose, conf) = self.refine(canvas, gray, pose)?;
                let sim = similarity(canvas, gray, pose);
                (sim >= MIN_SIMILARITY).then_some((pose, conf, sim))
            })
            .max_by(|a, b| a.2.total_cmp(&b.2))
    }
}

/// Normalised cross-correlation between the frame and `canvas` at `pose`
/// (centre of the frame only; 0 when the mosaic does not cover it enough).
pub(super) fn similarity(canvas: &Canvas, gray: &Gray, pose: Pose) -> f32 {
    let (reference, coverage) = canvas.sample_gray(pose, REG_W, REG_H, STEP);
    if coverage < 0.35 {
        return 0.0;
    }
    ncc(&reference, gray, gray.w / 8, gray.h / 8)
}

/// Candidates kept by the thumbnail comparison for a proper registration.
const SHORTLIST: usize = 16;
/// Candidates around the anchor tried per frame, in addition, when the mouse
/// is expected near it (after a lift-off). Thumbnails are coarse: on regular
/// text they may miss the right place when it is close.
const NEAR_BATCH: usize = 33;
/// Thumbnail size: the registration window downsampled 8 times.
const THUMB: (usize, usize) = ((REG_W / 8) as usize, (REG_H / 8) as usize);

/// Thumbnails of a mosaic seen from a grid of poses, to find quickly where a
/// frame may be. The grid covers the whole mosaic in the axes of the anchor
/// (where the mouse was last seen), with a range of rotations. Built once,
/// when first needed: the mosaic does not change while it is searched.
#[derive(Default)]
pub(super) struct SearchIndex {
    anchor: Pose,
    /// Poses around the anchor, nearest first, tried in turn (empty if the
    /// mouse is not expected near the anchor).
    near: Vec<Pose>,
    near_next: usize,
    built: bool,
    poses: Vec<Pose>,
    /// Zero-mean, unit-norm thumbnails, one per pose.
    thumbs: Vec<[f32; THUMB.0 * THUMB.1]>,
}

impl SearchIndex {
    /// `near`: the mouse is probably close to `anchor` (put down again after a lift-off).
    pub(super) fn new(anchor: Pose, near: bool) -> Self {
        let mut s = Self { anchor, ..Default::default() };
        if near {
            // Offsets of a third of the frame around the anchor, smallest first.
            let (ox, oy) = (REG_W as f64 / 3.0, REG_H as f64 / 3.0);
            let mut offsets: Vec<(f64, f64)> =
                (-1..=1).flat_map(|j| (-1..=1).map(move |i| (i as f64 * ox, j as f64 * oy))).collect();
            offsets.sort_by(|a, b| a.0.hypot(a.1).total_cmp(&b.0.hypot(b.1)));
            s.near = ANGLES
                .iter()
                .flat_map(|a| offsets.iter().map(move |&(du, dv)| anchor.moved_local(du, dv, a.to_radians())))
                .collect();
        }
        s
    }

    /// Stops trying the poses around the anchor (keeps the thumbnails).
    pub(super) fn without_near(mut self) -> Self {
        self.near = Vec::new();
        self
    }

    /// The `n` candidates whose thumbnails look most like the frame, and the
    /// next few around the anchor.
    fn best(&mut self, canvas: &Canvas, gray: &Gray, n: usize) -> Vec<Pose> {
        self.build(canvas);
        let mut out = Vec::new();
        if !self.near.is_empty() {
            let start = if self.near_next >= self.near.len() { 0 } else { self.near_next };
            self.near_next = (start + NEAR_BATCH).min(self.near.len());
            out.extend_from_slice(&self.near[start..self.near_next]);
        }
        let Some(t) = thumbnail(gray) else { return out };
        let mut scored: Vec<(f32, usize)> = self
            .thumbs
            .par_iter()
            .enumerate()
            .map(|(i, c)| (c.iter().zip(&t).map(|(a, b)| a * b).sum::<f32>(), i))
            .collect();
        let n = n.min(scored.len());
        if n > 0 {
            scored.select_nth_unstable_by(n - 1, |a, b| b.0.total_cmp(&a.0));
            scored.truncate(n);
            scored.sort_by(|a, b| b.0.total_cmp(&a.0));
            out.extend(scored.iter().map(|&(_, i)| self.poses[i]));
        }
        out
    }

    /// Computes the thumbnails, if not done yet (`canvas` must not have changed since).
    pub(super) fn build(&mut self, canvas: &Canvas) {
        if self.built {
            return;
        }
        self.built = true;
        let Some(b) = canvas.bounds() else { return };
        let anchor = self.anchor;
        // Steps of a sixth of the frame: thumbnails only match when the offset is small.
        let (ox, oy) = (REG_W as f64 / 6.0, REG_H as f64 / 6.0);
        // Extent of the mosaic in the anchor's axes.
        let corners =
            [(b.x0, b.y0), (b.x1, b.y0), (b.x0, b.y1), (b.x1, b.y1)].map(|(x, y)| anchor.inverse(x as f64, y as f64));
        let (u0, u1) = corners.iter().fold((f64::MAX, f64::MIN), |a, c| (a.0.min(c.0), a.1.max(c.0)));
        let (v0, v1) = corners.iter().fold((f64::MAX, f64::MIN), |a, c| (a.0.min(c.1), a.1.max(c.1)));
        let mut poses = Vec::new();
        for j in (v0 / oy).floor() as i32..=(v1 / oy).ceil() as i32 {
            for i in (u0 / ox).floor() as i32..=(u1 / ox).ceil() as i32 {
                for a in ANGLES {
                    poses.push(anchor.moved_local(i as f64 * ox, j as f64 * oy, a.to_radians()));
                }
            }
        }
        let thumbs: Vec<(Pose, Option<[f32; THUMB.0 * THUMB.1]>)> = poses
            .into_par_iter()
            .map(|p| {
                let (g, coverage) = canvas.sample_gray(p, REG_W, REG_H, STEP * 4);
                (p, if coverage >= 0.4 { thumbnail(&g) } else { None })
            })
            .collect();
        for (p, t) in thumbs {
            if let Some(t) = t {
                self.poses.push(p);
                self.thumbs.push(t);
            }
        }
    }
}

/// Zero-mean, unit-norm thumbnail of a registration image (any power-of-two
/// fraction of the window size); `None` for a blank image.
fn thumbnail(g: &Gray) -> Option<[f32; THUMB.0 * THUMB.1]> {
    let k = g.w / THUMB.0;
    debug_assert!(k >= 1 && g.h / k == THUMB.1);
    let mut t = [0f32; THUMB.0 * THUMB.1];
    for (i, v) in t.iter_mut().enumerate() {
        let (tx, ty) = (i % THUMB.0, i / THUMB.0);
        let mut acc = 0.0;
        for y in ty * k..(ty + 1) * k {
            acc += g.px[y * g.w + tx * k..][..k].iter().sum::<f32>();
        }
        *v = acc;
    }
    let mean = t.iter().sum::<f32>() / t.len() as f32;
    t.iter_mut().for_each(|v| *v -= mean);
    let norm = t.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm < 1e-3 {
        return None;
    }
    t.iter_mut().for_each(|v| *v /= norm);
    Some(t)
}

/// Normalised cross-correlation of two images of the same size, ignoring a
/// border of `bx` × `by` pixels.
fn ncc(a: &Gray, b: &Gray, bx: usize, by: usize) -> f32 {
    let rows = by..a.h - by;
    let cols = bx..a.w - bx;
    let n = (rows.len() * cols.len()) as f64;
    let (mut sa, mut sb) = (0.0f64, 0.0f64);
    for y in rows.clone() {
        for x in cols.clone() {
            sa += a.px[y * a.w + x] as f64;
            sb += b.px[y * b.w + x] as f64;
        }
    }
    let (ma, mb) = (sa / n, sb / n);
    let (mut sab, mut saa, mut sbb) = (0.0f64, 0.0f64, 0.0f64);
    for y in rows {
        for x in cols.clone() {
            let (da, db) = (a.px[y * a.w + x] as f64 - ma, b.px[y * b.w + x] as f64 - mb);
            sab += da * db;
            saa += da * da;
            sbb += db * db;
        }
    }
    if saa <= 0.0 || sbb <= 0.0 { 0.0 } else { (sab / (saa * sbb).sqrt()) as f32 }
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

    #[test]
    fn ncc_of_identical_and_inverted_images() {
        let mut a = Gray::new(16, 8);
        for (i, v) in a.px.iter_mut().enumerate() {
            *v = ((i * 37) % 11) as f32;
        }
        let mut b = a.clone();
        assert!((ncc(&a, &b, 1, 1) - 1.0).abs() < 1e-5);
        for v in &mut b.px {
            *v = 20.0 - *v;
        }
        assert!((ncc(&a, &b, 1, 1) + 1.0).abs() < 1e-5);
    }
}
