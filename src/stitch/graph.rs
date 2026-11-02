//! Global alignment of the scan (the "pose graph" of the original software).
//!
//! Tracking registers each frame on the mosaic built so far. Its small errors
//! add up along a pass, so when the mouse comes back over a part scanned
//! earlier the two passes no longer match exactly, and nothing done frame by
//! frame can fix that without a visible step. Instead, keyframes are kept
//! (every few tens of pixels) and matched directly against the keyframes of
//! earlier passes that they overlap. At the end, all poses are adjusted
//! together so that the tracked motions and these overlaps agree as well as
//! possible, and the final image is rendered again from the keyframes.

use std::io::Cursor;
use std::ops::Range;

use image::RgbImage;
use image::codecs::jpeg::JpegEncoder;
use rayon::prelude::*;

use super::canvas::{Canvas, Pose};
use super::locate::Matcher;
use super::register::Gray;
use super::stitcher::{REG_H, REG_W, STEP};

/// Distance travelled between two keyframes (pixels; frames are 640×320).
pub(super) const KEY_SPACING: f64 = 40.0;
/// Rotation between two keyframes (radians).
pub(super) const KEY_TURN: f64 = 0.03;
/// Weight of the tracked motion between two keyframes when the mouse was
/// lifted or tracking was unsure in between (1 otherwise).
pub(super) const WEAK_LINK: f64 = 0.2;
/// Keyframes of the same chain closer in time than this (frames) are linked
/// by tracking only: their overlap says nothing tracking does not already.
const LOOP_MIN_GAP: u64 = 45;
/// Earlier keyframes a keyframe is matched against, at most (taken from
/// different moments of the scan).
const LOOP_CANDIDATES: usize = 3;
/// Largest correction of the tracked relative pose accepted for an overlap.
const LOOP_MAX_SHIFT: f64 = 120.0;
const LOOP_MAX_TURN: f64 = 0.1;
/// Part of a keyframe the other one must cover.
const LOOP_MIN_OVERLAP: f32 = 0.3;
/// Normalised cross-correlation of the overlap required to accept a match;
/// matches one text line off score well below.
const LOOP_MIN_NCC: f32 = 0.75;
/// Disagreement (pixels, at the frame edge) beyond which an overlap is taken
/// for a wrong match once all poses are adjusted.
const MAX_RESIDUAL: f64 = 8.0;
/// Weight pulling each pose toward the tracked one. Tiny: it only keeps the
/// problem well posed when parts of the scan are not linked to each other.
const PRIOR: f64 = 1e-6;
/// Distance between two frames kept for the final image between keyframes:
/// keyframes alone leave gaps at the rounded corners of the scan window.
pub(super) const FRAME_SPACING: f64 = 12.0;
/// JPEG quality of the stored frames.
const QUALITY: u8 = 92;

/// Size of the downsampled registration window.
const GRAY: (usize, usize) = ((REG_W / STEP) as usize, (REG_H / STEP) as usize);

pub(super) struct Key {
    /// Frame number (for the rollback on lift-off).
    pub frame: u64,
    /// Pose given by tracking.
    pub pose: Pose,
    /// Weight of the tracked motion from the previous keyframe; `None` for the
    /// first keyframe of a chain (fragment), not linked to the previous one.
    link: Option<f64>,
    /// Chain number: keyframes linked by tracking share it.
    chain: u32,
    /// Registration window, downsampled by `STEP`.
    gray: Box<[u8]>,
    /// The frame as blended (corrected, oriented), JPEG-compressed.
    jpeg: Vec<u8>,
    /// Frames blended after this keyframe and before the next one.
    following: Vec<Following>,
    /// Earlier keyframes still to be searched for overlaps.
    pending: Option<Range<usize>>,
}

/// A frame rendered relative to the keyframe before it.
struct Following {
    frame: u64,
    /// Pose in the keyframe's axes, as tracked.
    rel: Pose,
    jpeg: Vec<u8>,
}

fn compress(img: &RgbImage) -> Option<Vec<u8>> {
    let mut jpeg = Vec::new();
    match img.write_with_encoder(JpegEncoder::new_with_quality(Cursor::new(&mut jpeg), QUALITY)) {
        Ok(()) => Some(jpeg),
        Err(e) => {
            log::warn!("image non enregistrée pour l'alignement : {e}");
            None
        }
    }
}

fn decompress(jpeg: &[u8]) -> Option<RgbImage> {
    image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).ok().map(|i| i.into_rgb8())
}

/// Measured pose of keyframe `b` in the axes of keyframe `a`.
#[derive(Clone, Copy, Debug)]
struct Edge {
    a: usize,
    b: usize,
    rel: Pose,
    weight: f64,
}

#[derive(Default)]
pub(super) struct PoseGraph {
    keys: Vec<Key>,
    /// Overlaps found between keyframes.
    loops: Vec<Edge>,
}

/// Pose of `b` in the axes of `a`.
fn relative(a: Pose, b: Pose) -> Pose {
    let (x, y) = a.inverse(b.x, b.y);
    Pose { x, y, theta: b.theta - a.theta }
}

impl PoseGraph {
    pub(super) fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub(super) fn last(&self) -> Option<&Key> {
        self.keys.last()
    }

    /// Pose of the last frame stored (keyframe or following frame).
    pub(super) fn last_stored(&self) -> Option<Pose> {
        let k = self.keys.last()?;
        Some(k.following.last().map_or(k.pose, |f| k.pose.moved_local(f.rel.x, f.rel.y, f.rel.theta)))
    }

    /// Stores a frame for the final image, relative to the last keyframe.
    pub(super) fn add_frame(&mut self, frame: u64, pose: Pose, img: &RgbImage) {
        let Some(k) = self.keys.last_mut() else { return };
        if let Some(jpeg) = compress(img) {
            k.following.push(Following { frame, rel: relative(k.pose, pose), jpeg });
        }
    }

    /// Adds a keyframe. `link`: weight of the tracked motion from the previous
    /// keyframe (`None`: not linked).
    pub(super) fn push(&mut self, frame: u64, pose: Pose, link: Option<f64>, img: &RgbImage, gray: &Gray) {
        let Some(jpeg) = compress(img) else { return };
        let link = link.filter(|_| !self.keys.is_empty());
        let chain = match self.keys.last() {
            Some(k) if link.is_some() => k.chain,
            Some(k) => k.chain + 1,
            None => 0,
        };
        let n = self.keys.len();
        self.keys.push(Key {
            frame,
            pose,
            link,
            chain,
            gray: gray.px.iter().map(|&v| v.round().clamp(0.0, 255.0) as u8).collect(),
            jpeg,
            following: Vec::new(),
            pending: Some(0..n),
        });
    }

    /// Drops the keyframes taken at or after `frame` (taken back on lift-off).
    pub(super) fn rollback(&mut self, frame: u64) {
        let n = self.keys.iter().position(|k| k.frame >= frame).unwrap_or(self.keys.len());
        self.keys.truncate(n);
        if let Some(k) = self.keys.last_mut() {
            k.following.retain(|f| f.frame < frame);
        }
        self.loops.retain(|e| e.a < n && e.b < n);
    }

    /// Appends the keyframes of another fragment, whose coordinates `transform`
    /// maps onto ours. Its keyframes are then searched on ours for overlaps.
    pub(super) fn append(&mut self, other: PoseGraph, transform: Pose) {
        let (n, chains) = (self.keys.len(), self.keys.last().map_or(0, |k| k.chain + 1));
        self.keys.extend(other.keys.into_iter().map(|k| Key {
            pose: transform.transform(k.pose),
            chain: k.chain + chains,
            // Already searched among its own fragment.
            pending: Some(0..n),
            ..k
        }));
        self.loops.extend(other.loops.into_iter().map(|e| Edge { a: e.a + n, b: e.b + n, ..e }));
    }

    /// Searches overlaps for at most `budget` keyframes (spread over the scan
    /// so that the end does not take long). `mask`: valid pixels of a keyframe.
    pub(super) fn search(&mut self, matcher: &Matcher, mask: &[bool], budget: usize) {
        for _ in 0..budget {
            let Some(j) = self.keys.iter().position(|k| k.pending.is_some()) else { return };
            let range = self.keys[j].pending.take().unwrap_or_default();
            let found = self.overlaps(j, range, matcher, mask);
            self.loops.extend(found);
        }
    }

    /// Overlaps of keyframe `j` with keyframes `range`.
    fn overlaps(&self, j: usize, range: Range<usize>, matcher: &Matcher, mask: &[bool]) -> Vec<Edge> {
        let kj = &self.keys[j];
        let mut cand: Vec<(f64, usize, Pose)> = range
            .filter(|&i| i != j)
            .filter_map(|i| {
                let ki = &self.keys[i];
                if ki.chain == kj.chain && ki.frame.abs_diff(kj.frame) < LOOP_MIN_GAP {
                    return None;
                }
                let rel = relative(ki.pose, kj.pose);
                let (nx, ny) = (rel.x / REG_W as f64, rel.y / REG_H as f64);
                (nx.abs() < 0.5 && ny.abs() < 0.5 && rel.theta.abs() < 0.35).then_some((nx * nx + ny * ny, i, rel))
            })
            .collect();
        cand.sort_by(|a, b| a.0.total_cmp(&b.0));
        // The closest ones, from different moments of the scan.
        let mut picked: Vec<(usize, Pose)> = Vec::new();
        for (_, i, rel) in cand {
            if picked.len() == LOOP_CANDIDATES {
                break;
            }
            if picked.iter().all(|&(p, _)| self.keys[p].frame.abs_diff(self.keys[i].frame) >= LOOP_MIN_GAP) {
                picked.push((i, rel));
            }
        }
        let cur = to_gray(&kj.gray, mask);
        picked
            .into_iter()
            .filter_map(|(i, predicted)| {
                let rel = register(&self.keys[i], &cur, predicted, matcher, mask)?;
                Some(Edge { a: i, b: j, rel, weight: 1.0 })
            })
            .collect()
    }

    /// Adjusted poses of all keyframes (searching the overlaps not searched yet).
    pub(super) fn solve(&self, matcher: &Matcher, mask: &[bool]) -> Vec<Pose> {
        let mut loops = self.loops.clone();
        let pending: Vec<(usize, Range<usize>)> =
            self.keys.iter().enumerate().filter_map(|(j, k)| k.pending.clone().map(|r| (j, r))).collect();
        loops.extend(
            pending.into_par_iter().flat_map_iter(|(j, r)| self.overlaps(j, r, matcher, mask)).collect::<Vec<_>>(),
        );
        let tracked: Vec<Edge> = self
            .keys
            .iter()
            .enumerate()
            .filter_map(|(j, k)| {
                let w = k.link?;
                Some(Edge { a: j - 1, b: j, rel: relative(self.keys[j - 1].pose, k.pose), weight: w })
            })
            .collect();
        let initial: Vec<Pose> = self.keys.iter().map(|k| k.pose).collect();
        let n_loops = loops.len();
        let poses = adjust_robust(&initial, &tracked, loops);
        log::info!("alignement global : {} images clés, {n_loops} recouvrements", self.keys.len());
        poses
    }

    /// Mosaic of the stored frames, the keyframes being at `poses`.
    pub(super) fn render(&self, poses: &[Pose], weights: &[f32]) -> Canvas {
        // In the order they were scanned; following frames move with their keyframe.
        let frames: Vec<(&[u8], Pose)> = self
            .keys
            .iter()
            .zip(poses)
            .flat_map(|(k, &p)| {
                std::iter::once((&k.jpeg[..], p))
                    .chain(k.following.iter().map(move |f| (&f.jpeg[..], p.moved_local(f.rel.x, f.rel.y, f.rel.theta))))
            })
            .collect();
        let mut canvas = Canvas::new();
        for chunk in frames.chunks(32) {
            let images: Vec<Option<RgbImage>> = chunk.par_iter().map(|(jpeg, _)| decompress(jpeg)).collect();
            for (img, &(_, pose)) in images.iter().zip(chunk) {
                if let Some(img) = img
                    && img.len() == weights.len() * 3
                {
                    canvas.blend(img, weights, pose);
                }
            }
        }
        canvas
    }
}

/// Grey level image of a keyframe, invalid pixels (dark border of the scan
/// window) set to the mean of the others.
fn to_gray(px: &[u8], mask: &[bool]) -> Gray {
    let mut g = Gray::new(GRAY.0, GRAY.1);
    let (mut sum, mut n) = (0.0, 0);
    for ((o, &p), &m) in g.px.iter_mut().zip(px).zip(mask) {
        if m {
            *o = p as f32;
            sum += p as f32;
            n += 1;
        }
    }
    let mean = if n > 0 { sum / n as f32 } else { 0.0 };
    for (o, &m) in g.px.iter_mut().zip(mask) {
        if !m {
            *o = mean;
        }
    }
    g
}

/// Keyframe `a` as seen by a frame at `rel` (pose in `a`'s axes): the image,
/// which of its pixels come from `a`, and their share.
fn warp(a: &Key, rel: Pose, mask: &[bool]) -> (Gray, Vec<bool>, f32) {
    let (gw, gh) = GRAY;
    let s = STEP as f64;
    let (cx, cy) = (REG_W as f64 / 2.0, REG_H as f64 / 2.0);
    let mut g = Gray::new(gw, gh);
    let mut valid = vec![false; gw * gh];
    let (mut sum, mut n) = (0.0f32, 0usize);
    for y in 0..gh {
        for x in 0..gw {
            let (ua, va) = rel.apply((x as f64 + 0.5) * s - cx, (y as f64 + 0.5) * s - cy);
            let (fx, fy) = ((ua + cx) / s - 0.5, (va + cy) / s - 0.5);
            let (x0, y0) = (fx.floor(), fy.floor());
            let (ax, ay) = ((fx - x0) as f32, (fy - y0) as f32);
            let (x0, y0) = (x0 as i64, y0 as i64);
            let (mut acc, mut wsum) = (0.0f32, 0.0f32);
            for (dx, dy, w) in
                [(0, 0, (1.0 - ax) * (1.0 - ay)), (1, 0, ax * (1.0 - ay)), (0, 1, (1.0 - ax) * ay), (1, 1, ax * ay)]
            {
                let (xx, yy) = (x0 + dx, y0 + dy);
                if xx >= 0 && yy >= 0 && (xx as usize) < gw && (yy as usize) < gh {
                    let i = yy as usize * gw + xx as usize;
                    if mask[i] {
                        acc += a.gray[i] as f32 * w;
                        wsum += w;
                    }
                }
            }
            if wsum > 0.5 {
                let v = acc / wsum;
                g.px[y * gw + x] = v;
                valid[y * gw + x] = true;
                sum += v;
                n += 1;
            }
        }
    }
    let mean = if n > 0 { sum / n as f32 } else { 0.0 };
    for (v, &ok) in g.px.iter_mut().zip(&valid) {
        if !ok {
            *v = mean;
        }
    }
    (g, valid, n as f32 / (gw * gh) as f32)
}

/// Pose of the keyframe whose image is `cur` in the axes of keyframe `a`,
/// starting from `predicted`; `None` unless the overlap really matches.
fn register(a: &Key, cur: &Gray, predicted: Pose, matcher: &Matcher, mask: &[bool]) -> Option<Pose> {
    let mut rel = predicted;
    for pass in 0..2 {
        let (reference, _, coverage) = warp(a, rel, mask);
        if coverage < LOOP_MIN_OVERLAP {
            return None;
        }
        let (du, dv, dt, _) = matcher.estimate(&reference, cur)?;
        // The second pass only refines.
        if pass == 1 && (du.abs() > 4.0 || dv.abs() > 4.0) {
            return None;
        }
        rel = rel.moved_local(du, dv, dt);
    }
    if (rel.x - predicted.x).hypot(rel.y - predicted.y) > LOOP_MAX_SHIFT
        || (rel.theta - predicted.theta).abs() > LOOP_MAX_TURN
    {
        return None;
    }
    let (reference, valid, coverage) = warp(a, rel, mask);
    let both: Vec<bool> = valid.iter().zip(mask).map(|(a, b)| *a && *b).collect();
    (coverage >= LOOP_MIN_OVERLAP && ncc(&reference, cur, &both) >= LOOP_MIN_NCC).then_some(rel)
}

/// Normalised cross-correlation of two images over the pixels of `mask`.
fn ncc(a: &Gray, b: &Gray, mask: &[bool]) -> f32 {
    let (mut n, mut sa, mut sb) = (0.0f64, 0.0f64, 0.0f64);
    for ((&x, &y), &m) in a.px.iter().zip(&b.px).zip(mask) {
        if m {
            n += 1.0;
            sa += x as f64;
            sb += y as f64;
        }
    }
    if n < 100.0 {
        return 0.0;
    }
    let (ma, mb) = (sa / n, sb / n);
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for ((&x, &y), &m) in a.px.iter().zip(&b.px).zip(mask) {
        if m {
            let (dx, dy) = (x as f64 - ma, y as f64 - mb);
            sab += dx * dy;
            saa += dx * dx;
            sbb += dy * dy;
        }
    }
    if saa <= 0.0 || sbb <= 0.0 { 0.0 } else { (sab / (saa * sbb).sqrt()) as f32 }
}

/// Disagreement between edge `e` and `poses`, in pixels at the frame edge.
fn residual(e: &Edge, poses: &[Pose]) -> f64 {
    let r = relative(poses[e.a], poses[e.b]);
    (r.x - e.rel.x).hypot(r.y - e.rel.y) + (r.theta - e.rel.theta).abs() * REG_W as f64 / 2.0
}

/// [`adjust`], dropping the overlaps that disagree with the others (wrong
/// matches) until the remaining ones agree.
fn adjust_robust(initial: &[Pose], tracked: &[Edge], mut loops: Vec<Edge>) -> Vec<Pose> {
    loop {
        let edges: Vec<Edge> = tracked.iter().chain(&loops).copied().collect();
        let poses = adjust(initial, &edges);
        let res: Vec<f64> = loops.iter().map(|e| residual(e, &poses)).collect();
        let worst = res.iter().copied().fold(0.0, f64::max);
        if worst <= MAX_RESIDUAL {
            return poses;
        }
        // The worst first: a wrong match also pulls good ones away.
        let limit = (worst * 0.7).max(MAX_RESIDUAL);
        let before = loops.len();
        loops = loops.into_iter().zip(&res).filter(|&(_, &r)| r < limit).map(|(e, _)| e).collect();
        log::debug!("alignement global : {} recouvrement(s) écarté(s)", before - loops.len());
    }
}

/// Poses that best fit the edges (least squares), close to `initial`. The
/// rotations are solved first; with them fixed, the translations are linear.
fn adjust(initial: &[Pose], edges: &[Edge]) -> Vec<Pose> {
    let n = initial.len();
    // Rotations: θb − θa = rel.θ.
    let mut b = initial.iter().map(|p| PRIOR * p.theta).collect::<Vec<_>>();
    for e in edges {
        b[e.b] += e.weight * e.rel.theta;
        b[e.a] -= e.weight * e.rel.theta;
    }
    let theta = solve_laplacian(n, edges, &b, &initial.iter().map(|p| p.theta).collect::<Vec<_>>());
    // Translations: tb − ta = R(θa)·rel.t.
    let (mut bx, mut by) = (
        initial.iter().map(|p| PRIOR * p.x).collect::<Vec<_>>(),
        initial.iter().map(|p| PRIOR * p.y).collect::<Vec<_>>(),
    );
    for e in edges {
        let (s, c) = theta[e.a].sin_cos();
        let (dx, dy) = (c * e.rel.x - s * e.rel.y, s * e.rel.x + c * e.rel.y);
        bx[e.b] += e.weight * dx;
        bx[e.a] -= e.weight * dx;
        by[e.b] += e.weight * dy;
        by[e.a] -= e.weight * dy;
    }
    let (x, y) = rayon::join(
        || solve_laplacian(n, edges, &bx, &initial.iter().map(|p| p.x).collect::<Vec<_>>()),
        || solve_laplacian(n, edges, &by, &initial.iter().map(|p| p.y).collect::<Vec<_>>()),
    );
    (0..n).map(|i| Pose { x: x[i], y: y[i], theta: theta[i] }).collect()
}

/// Solves (L + PRIOR·I)·x = b, L being the weighted Laplacian of the edges,
/// by preconditioned conjugate gradients from `x0`.
fn solve_laplacian(n: usize, edges: &[Edge], b: &[f64], x0: &[f64]) -> Vec<f64> {
    let mul = |x: &[f64]| {
        let mut y: Vec<f64> = x.iter().map(|v| PRIOR * v).collect();
        for e in edges {
            let d = e.weight * (x[e.b] - x[e.a]);
            y[e.b] += d;
            y[e.a] -= d;
        }
        y
    };
    let mut diag = vec![PRIOR; n];
    for e in edges {
        diag[e.a] += e.weight;
        diag[e.b] += e.weight;
    }
    let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
    let mut x = x0.to_vec();
    let mut r: Vec<f64> = b.iter().zip(mul(&x)).map(|(b, ax)| b - ax).collect();
    let mut z: Vec<f64> = r.iter().zip(&diag).map(|(r, d)| r / d).collect();
    let mut p = z.clone();
    let mut rz = dot(&r, &z);
    let tol = 1e-20 * dot(b, b).max(1e-30);
    for _ in 0..(50 * n).max(100) {
        if dot(&r, &r) <= tol {
            break;
        }
        let ap = mul(&p);
        let alpha = rz / dot(&p, &ap);
        for i in 0..n {
            x[i] += alpha * p[i];
            r[i] -= alpha * ap[i];
        }
        z = r.iter().zip(&diag).map(|(r, d)| r / d).collect();
        let rz_new = dot(&r, &z);
        let beta = rz_new / rz;
        rz = rz_new;
        for i in 0..n {
            p[i] = z[i] + beta * p[i];
        }
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(a: usize, b: usize, pa: Pose, pb: Pose) -> Edge {
        Edge { a, b, rel: relative(pa, pb), weight: 1.0 }
    }

    /// A pass that drifted is brought back onto the first one by their overlap,
    /// smoothly along the pass; a wrong overlap is ignored.
    #[test]
    fn overlap_corrects_drift() {
        // Out along y = 0, back along y = 100 (truth), each step 40 px.
        let truth: Vec<Pose> = (0..20)
            .map(|i| {
                if i < 10 {
                    Pose { x: i as f64 * 40.0, y: 0.0, theta: 0.0 }
                } else {
                    Pose { x: (19 - i) as f64 * 40.0, y: 100.0, theta: 0.0 }
                }
            })
            .collect();
        // Tracking drifts by 1 px per step along y on the way back.
        let tracked: Vec<Pose> =
            truth.iter().enumerate().map(|(i, p)| Pose { y: p.y + i.saturating_sub(10) as f64, ..*p }).collect();
        let odo: Vec<Edge> = (1..20).map(|i| edge(i - 1, i, tracked[i - 1], tracked[i])).collect();
        // True overlaps between the passes, and one wrong match (a text line off).
        let mut loops: Vec<Edge> = (0..10).map(|i| edge(i, 19 - i, truth[i], truth[19 - i])).collect();
        let mut wrong = edge(5, 14, truth[5], truth[14]);
        wrong.rel.y += 40.0;
        loops.push(wrong);
        let poses = adjust_robust(&tracked, &odo, loops);
        for i in 10..20 {
            let r = relative(poses[19 - i], poses[i]);
            let t = relative(truth[19 - i], truth[i]);
            assert!((r.y - t.y).abs() < 1.5, "key {i}: {:.1} instead of {:.1}", r.y, t.y);
        }
    }
}
