//! Frame registration and mosaicking.
//!
//! The mouse can be lifted and put down anywhere. After a lift-off the
//! position is searched on the whole mosaic; if it cannot be found (the mouse
//! was put down on a part of the page not scanned yet), scanning goes on in a
//! separate fragment, and the fragments are merged as soon as a frame is found
//! on both.
//!
//! Tracking is local: where passes overlap, their small disagreement is fixed
//! at the end by the global alignment of keyframes (see [`super::graph`]),
//! from which the final image is rendered.

use std::collections::VecDeque;

use image::RgbImage;

use super::StitchSettings;
use super::canvas::{self, Bounds, Canvas, Pose, Undo};
use super::flat::{AutoFlat, FlatField};
use super::graph::{FRAME_SPACING, KEY_SPACING, KEY_TURN, PoseGraph, WEAK_LINK};
use super::laser::LaserOdometry;
use super::lift::LiftDetector;
use super::locate::{MIN_SIMILARITY, Matcher, SearchIndex, similarity};
use super::output::{auto_white_balance, orient_image, scanned_area};
use super::register::Gray;
use crate::device::{FRAME_HEIGHT, FRAME_WIDTH, Frame, MouseData};

/// The camera sees the page at an angle (through a mirror): a frame pixel
/// covers more paper along v than along u. Measured with the laser sensors,
/// whose counts per pixel differ by ~8.4 % between the two axes — the same
/// for both sensors. Frames are stretched along v to square pixels before
/// stitching; otherwise rotating the mouse is not a rigid motion of the image.
/// The measured ratio is 1.084; 320 rows (1.081) keep the FFT sizes small.
pub const STITCH_FRAME: (u32, u32) = (FRAME_WIDTH, 320);
/// Downsampling factor used for registration.
pub(super) const STEP: u32 = 2;
/// Centred part of the frame used for registration. Its dimensions are rounded
/// down to multiples of 16 so that every FFT size (full, half and coarse images)
/// only has small prime factors (148 = 4 × 37 rows, for instance, makes the FFT
/// several times slower).
pub(super) const REG_W: u32 = STITCH_FRAME.0 & !15;
pub(super) const REG_H: u32 = STITCH_FRAME.1 & !15;
// The registration window must be centred exactly on the frame centre.
const _: () = assert!((STITCH_FRAME.0 - REG_W).is_multiple_of(2) && (STITCH_FRAME.1 - REG_H).is_multiple_of(2));
/// Phase-correlation confidence considered reliable.
pub(super) const MIN_CONFIDENCE: f32 = 7.0;

/// Largest disagreement tolerated between image registration and the laser
/// measurement (pixels, radians). Beyond it the image locked onto a wrong
/// match — typically one period away on ruled lines or table borders.
const LASER_TOLERANCE: (f64, f64) = (24.0, 0.04);

/// Frames blended this recently are taken back when a lift-off is detected:
/// while the mouse tilts and leaves the page the image is already blurred
/// and distorted, before the detector can tell (the original software also
/// "rolls back" its last images on lift-off).
const ROLLBACK_FRAMES: u64 = 4;

/// After a relocalization, frames that must be found again on the mosaic
/// before blending resumes (likewise before two fragments are merged). A
/// wrong match rarely survives the following frames.
const CONFIRM_FRAMES: u32 = 3;
/// Registration confidence required during that confirmation.
const CONFIRM_CONFIDENCE: f32 = MIN_CONFIDENCE * 2.0;
/// Keyframes searched for overlaps with earlier passes per frame, while
/// scanning (the rest is done at the end).
const OVERLAP_SEARCH_PER_FRAME: usize = 1;
/// Frames between two attempts to find the current frame on a parked fragment.
const ATTACH_EVERY: u64 = 3;
/// Frames of unsuccessful search after which scanning continues on a new fragment.
const DETACH_AFTER: u32 = 6;

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
    /// The current position could not be found on the main mosaic: scanning
    /// goes on separately until the mouse passes over a scanned part again.
    pub detached: bool,
}

struct RecentBlend {
    frame: u64,
    undo: Undo,
    /// `last_blend` before this blend.
    prev_blend: Option<Pose>,
}

/// A part of the mosaic not attached to the current one (yet).
struct Fragment {
    canvas: Canvas,
    graph: PoseGraph,
    queue: SearchIndex,
}

/// A candidate attachment of the current fragment to a parked one, being confirmed.
struct Attach {
    index: usize,
    /// Maps current canvas coordinates to the parked fragment's.
    transform: Pose,
    confirmed: u32,
}

pub struct Stitcher {
    pub settings: StitchSettings,
    pub flat: Option<FlatField>,
    auto_flat: AutoFlat,
    /// The fragment being scanned.
    canvas: Canvas,
    /// Its keyframes, for the global alignment.
    graph: PoseGraph,
    /// Weight of the tracked motion from the last keyframe to the next one.
    link: f64,
    /// Pixels of the registration window (downsampled) that see the page.
    key_mask: Vec<bool>,
    /// Other fragments, waiting to be attached to the current one.
    parked: Vec<Fragment>,
    attach: Option<Attach>,
    weights: Vec<f32>,
    weights_dims: (u32, u32),
    matcher: Matcher,
    lift: LiftDetector,
    laser: LaserOdometry,
    /// Set after a lift-off or a prolonged loss: the position must be found again.
    search: Option<SearchIndex>,
    /// Frames searched without success.
    search_frames: u32,
    /// Frames still to be confirmed after a relocalization (nothing is blended meanwhile).
    confirming: u32,
    /// Pose before the relocalization, to search again from it if it is not confirmed.
    search_anchor: Pose,
    pose: Pose,
    /// Smoothed translation per frame, in the frame's axes. Rotation is never
    /// extrapolated: when it cannot be measured (only one half of the frame has
    /// texture) a predicted rotation would feed back into itself and drift.
    velocity: (f64, f64),
    last_blend: Option<Pose>,
    /// Recent blends that can still be taken back.
    recent: VecDeque<RecentBlend>,
    prev_gray: Option<Gray>,
    lost_frames: u32,
    frames: u64,
}

impl Stitcher {
    pub fn new(settings: StitchSettings) -> Self {
        Self {
            settings,
            flat: None,
            auto_flat: AutoFlat::new(FRAME_WIDTH, FRAME_HEIGHT),
            canvas: Canvas::new(),
            graph: PoseGraph::default(),
            link: 1.0,
            key_mask: Vec::new(),
            parked: Vec::new(),
            attach: None,
            weights: Vec::new(),
            weights_dims: (0, 0),
            matcher: Matcher::new(),
            lift: LiftDetector::default(),
            laser: LaserOdometry::default(),
            search: None,
            search_frames: 0,
            confirming: 0,
            search_anchor: Pose::default(),
            pose: Pose::default(),
            velocity: (0.0, 0.0),
            last_blend: None,
            recent: VecDeque::new(),
            prev_gray: None,
            lost_frames: 0,
            frames: 0,
        }
    }

    pub fn reset(&mut self) {
        let (settings, flat) = (self.settings, self.flat.take());
        *self = Self { flat, ..Self::new(settings) };
    }

    /// The fragment being scanned (the whole mosaic unless the mouse was put
    /// down on a part of the page not scanned yet).
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

    /// Whether a parked fragment is larger than the current one.
    fn detached(&self) -> bool {
        let size = |b: Option<Bounds>| b.map_or(0, |b| b.width() as u64 * b.height() as u64);
        let current = size(self.canvas.bounds());
        self.parked.iter().any(|f| size(f.canvas.bounds()) > current)
    }

    /// Blends a frame, keeping what is needed to take it back for a while,
    /// and keeps it as a keyframe when it has moved enough since the last one.
    fn blend(&mut self, img: &RgbImage, gray: &Gray, pose: Pose) {
        let undo = self.canvas.blend_undoable(img, &self.weights, pose);
        self.recent.push_back(RecentBlend { frame: self.frames, undo, prev_blend: self.last_blend });
        self.last_blend = Some(pose);
        let key = self.graph.last().is_none_or(|k| {
            (pose.x - k.pose.x).hypot(pose.y - k.pose.y) >= KEY_SPACING || (pose.theta - k.pose.theta).abs() >= KEY_TURN
        });
        if key {
            self.graph.push(self.frames, pose, Some(self.link), img, gray);
            self.link = 1.0;
        } else if self.graph.last_stored().is_some_and(|p| (pose.x - p.x).hypot(pose.y - p.y) >= FRAME_SPACING) {
            self.graph.add_frame(self.frames, pose, img);
        }
    }

    /// Takes back the frames blended during the last `ROLLBACK_FRAMES` frames,
    /// and returns to the last pose kept on the mosaic.
    fn rollback(&mut self) {
        if let Some(first) = self.recent.front() {
            self.graph.rollback(first.frame);
        }
        while let Some(r) = self.recent.pop_back() {
            self.canvas.undo(r.undo);
            self.last_blend = r.prev_blend;
            self.pose = r.prev_blend.unwrap_or_default();
        }
        self.link = WEAK_LINK;
    }

    /// Forgets blends too old to be taken back.
    fn expire_recent(&mut self) {
        while self.recent.front().is_some_and(|r| r.frame + ROLLBACK_FRAMES < self.frames) {
            self.recent.pop_front();
        }
    }

    /// Starts looking for the current position on the mosaic, from the last known pose.
    fn start_search(&mut self) {
        self.search = Some(SearchIndex::new(self.pose, true));
        self.search_frames = 0;
        self.velocity = (0.0, 0.0);
    }

    /// Parks the current fragment and starts a new one.
    fn detach(&mut self, queue: SearchIndex) {
        let canvas = std::mem::take(&mut self.canvas);
        let graph = std::mem::take(&mut self.graph);
        log::debug!("frame {}: nouveau fragment ({} en attente)", self.frames, self.parked.len() + 1);
        // The mouse is not around where it was lifted any more.
        self.parked.push(Fragment { canvas, graph, queue: queue.without_near() });
        self.attach = None;
        self.search = None;
        self.recent.clear();
        self.pose = Pose::default();
        self.last_blend = None;
        self.velocity = (0.0, 0.0);
        self.prev_gray = None;
    }

    /// While tracking, tries to find the current frame on a parked fragment;
    /// once confirmed, merges the current fragment into it.
    fn try_attach(&mut self, gray: &Gray, pose: Pose) {
        if self.parked.is_empty() {
            return;
        }
        if let Some(a) = self.attach.as_mut() {
            let frag = &self.parked[a.index];
            let found = self
                .matcher
                .refine(&frag.canvas, gray, a.transform.transform(pose))
                .filter(|&(p, _)| similarity(&frag.canvas, gray, p) >= MIN_SIMILARITY);
            match found {
                Some((p, _)) => {
                    a.transform = Pose::between(pose, p);
                    a.confirmed += 1;
                    if a.confirmed >= CONFIRM_FRAMES {
                        self.merge();
                    }
                }
                None => {
                    log::debug!("frame {}: raccordement au fragment {} non confirmé", self.frames, a.index);
                    self.attach = None;
                }
            }
            return;
        }
        // Every few frames (it is not free), one fragment at a time.
        if !self.frames.is_multiple_of(ATTACH_EVERY) {
            return;
        }
        let index = (self.frames / ATTACH_EVERY) as usize % self.parked.len();
        let frag = &mut self.parked[index];
        if let Some((p, _, _)) = self.matcher.search(&frag.canvas, &mut frag.queue, gray) {
            log::debug!("frame {}: candidat de raccordement au fragment {index}", self.frames);
            self.attach = Some(Attach { index, transform: Pose::between(pose, p), confirmed: 1 });
        }
    }

    /// Registers the frame on the mosaic, starting from `predicted` (two passes).
    fn register(&self, gray: &Gray, predicted: Pose) -> Option<(Pose, f32)> {
        const MIN_COVERAGE: f32 = 0.35;
        let (reference, coverage) = self.canvas.sample_gray(predicted, REG_W, REG_H, STEP);
        if coverage < MIN_COVERAGE {
            return None;
        }
        let (du, dv, dt, conf) = self.matcher.estimate(&reference, gray)?;
        let pose = predicted.moved_local(du, dv, dt);
        // Refinement pass at the corrected pose.
        let (reference, coverage) = self.canvas.sample_gray(pose, REG_W, REG_H, STEP);
        if coverage >= MIN_COVERAGE
            && let Some((du, dv, dt, conf)) = self.matcher.estimate(&reference, gray)
            && du.abs() < 8.0
            && dv.abs() < 8.0
        {
            return Some((pose.moved_local(du, dv, dt), conf));
        }
        Some((pose, conf))
    }

    /// Merges the current fragment into the parked one it was found on.
    fn merge(&mut self) {
        let Some(a) = self.attach.take() else { return };
        let target = self.parked.swap_remove(a.index);
        let current = std::mem::replace(&mut self.canvas, target.canvas);
        let graph = std::mem::replace(&mut self.graph, target.graph);
        self.graph.append(graph, a.transform);
        if let Some((img, weights, b)) = current.render_weighted() {
            let centre = (b.x0 as f64 + img.width() as f64 / 2.0, b.y0 as f64 + img.height() as f64 / 2.0);
            let place = a.transform.transform(Pose { x: centre.0, y: centre.1, theta: 0.0 });
            // Only where the target is blank: the fragments may have drifted
            // apart slightly away from where they were joined.
            self.canvas.fill(&img, &weights, place);
        }
        self.pose = a.transform.transform(self.pose);
        self.last_blend = self.last_blend.map(|p| a.transform.transform(p));
        self.recent.clear();
        log::info!("fragment raccordé ({} restant(s))", self.parked.len());
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
        // Downsampled registration window, as in `to_gray`.
        let (s, x0, y0) = (STEP as usize, w.saturating_sub(REG_W) as usize / 2, h.saturating_sub(REG_H) as usize / 2);
        let (gw, gh) = ((REG_W / STEP) as usize, (REG_H / STEP) as usize);
        self.key_mask = (0..gw * gh)
            .map(|i| wt[(y0 + (i / gw) * s + s / 2) * w as usize + x0 + (i % gw) * s + s / 2] >= 0.5)
            .collect();
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

    /// Adds a frame to the mosaic.
    pub fn push(&mut self, frame: &Frame) -> StepInfo {
        self.push_with_image(frame).0
    }

    /// Like [`Stitcher::push`]; also returns the frame as stitched (corrected
    /// and oriented like the mosaic, [`STITCH_FRAME`] pixels), for display.
    pub fn push_with_image(&mut self, frame: &Frame) -> (StepInfo, RgbImage) {
        let brightness = mean_brightness(&frame.image);
        let img = self.prepare(&frame.image);
        let info = self.push_measured(&img, Some(&frame.data), brightness);
        (info, img)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn push_image(&mut self, img: &RgbImage, data: Option<&MouseData>) -> StepInfo {
        let brightness = mean_brightness(img);
        self.push_measured(img, data, brightness)
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
        self.expire_recent();
        let lifted = self.lift.update(brightness, data.map(|d| d.surface_quality), self.settings.lift_sensitivity);
        if lifted {
            // The last frames were taken while the mouse was leaving the page.
            self.rollback();
            self.laser.reset();
            self.attach = None;
            if self.confirming > 0 {
                self.confirming = 0;
                self.pose = self.search_anchor;
            }
            // Nothing usable while the mouse is in the air; search on landing.
            if !self.canvas.is_empty() {
                if self.search.is_none() {
                    self.start_search();
                }
                // Prepare the search meanwhile (the mosaic does not change until then).
                if let Some(index) = self.search.as_mut() {
                    index.build(&self.canvas);
                }
            }
            self.prev_gray = None;
            return StepInfo {
                pose: self.pose,
                lifted: true,
                searching: self.search.is_some(),
                lost: true,
                detached: self.detached(),
                ..Default::default()
            };
        }
        if let Some(mut queue) = self.search.take() {
            if let Some((pose, conf, _)) = self.matcher.search(&self.canvas, &mut queue, &gray) {
                // Found: confirm on the next frames before blending again.
                self.confirming = CONFIRM_FRAMES;
                self.search_anchor = self.pose;
                self.lost_frames = 0;
                self.link = WEAK_LINK;
                self.pose = pose;
                self.velocity = (0.0, 0.0);
                self.prev_gray = Some(gray);
                return StepInfo { pose, confidence: conf, searching: true, ..Default::default() };
            }
            self.search_frames += 1;
            if self.search_frames < DETACH_AFTER {
                self.search = Some(queue);
                return StepInfo {
                    pose: self.pose,
                    searching: true,
                    lost: true,
                    detached: self.detached(),
                    ..Default::default()
                };
            }
            // Not on this fragment (or not recognisable): go on with a new one.
            self.detach(queue);
        }

        if self.canvas.is_empty() {
            self.pose = Pose::default();
            self.blend(img, &gray, Pose::default());
            self.prev_gray = Some(gray);
            return StepInfo { pose: self.pose, confidence: 1.0, detached: self.detached(), ..Default::default() };
        }

        // Prediction: laser sensors when available, otherwise constant velocity.
        let predicted = match laser {
            Some((du, dv, dtheta)) => self.pose.moved_local(du, dv, dtheta),
            None => self.pose.moved_local(self.velocity.0, self.velocity.1, 0.0),
        };

        // 1. Register against the mosaic (limits drift).
        let mut result = self.register(&gray, predicted);
        let mut from_mosaic = result.is_some();
        // 2. Fall back to frame-to-frame registration.
        if result.is_none()
            && let Some(prev) = self.prev_gray.take()
        {
            if let Some((du, dv, dt, conf)) = self.matcher.estimate(&prev, &gray) {
                result = Some((self.pose.moved_local(du, dv, dt), conf));
            }
            self.prev_gray = Some(prev);
        }

        // The laser is a measurement too: an image match far from it is wrong
        // (typically one period away on ruled lines, table borders or text lines).
        if laser.is_some()
            && let Some((p, _)) = result
        {
            let (du, dv) = predicted.inverse(p.x, p.y);
            if du.hypot(dv) > LASER_TOLERANCE.0 || (p.theta - predicted.theta).abs() > LASER_TOLERANCE.1 {
                result = None;
                from_mosaic = false;
            }
        }

        if self.confirming > 0 {
            match result {
                Some((pose, conf))
                    if from_mosaic
                        && conf >= CONFIRM_CONFIDENCE
                        && similarity(&self.canvas, &gray, pose) >= MIN_SIMILARITY =>
                {
                    self.confirming -= 1;
                    self.pose = pose;
                    self.prev_gray = Some(gray);
                    return StepInfo { pose, confidence: conf, searching: self.confirming > 0, ..Default::default() };
                }
                _ => {
                    // The relocalization was wrong: search again from where the mouse was lifted.
                    self.confirming = 0;
                    self.pose = self.search_anchor;
                    self.search = Some(SearchIndex::new(self.pose, true));
                    self.velocity = (0.0, 0.0);
                    self.prev_gray = None;
                    return StepInfo { pose: self.pose, searching: true, lost: true, ..Default::default() };
                }
            }
        }

        let (pose, confidence, lost, laser_only) = match result {
            Some((p, c)) => (p, c, false, false),
            None if laser.is_some() => (predicted, 0.0, false, true),
            None => (predicted, 0.0, true, false),
        };
        if lost || laser_only {
            self.link = WEAK_LINK;
        }
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
                self.start_search();
                return StepInfo {
                    pose: self.pose,
                    searching: true,
                    lost: true,
                    speed,
                    detached: self.detached(),
                    ..Default::default()
                };
            }
            // Decay the extrapolation so a stationary mouse does not drift away.
            self.velocity = (self.velocity.0 * 0.5, self.velocity.1 * 0.5);
        }
        self.pose = pose;

        // Blend when the frame brings new content (and the pose is trusted).
        let moved = self.last_blend.is_none_or(|b| {
            let (dx, dy) = (pose.x - b.x, pose.y - b.y);
            (dx * dx + dy * dy).sqrt() > 3.0 || (pose.theta - b.theta).abs() > 0.004
        });
        if moved && !lost && !too_fast {
            self.blend(img, &gray, pose);
        }
        if from_mosaic && result.is_some() && !too_fast {
            self.try_attach(&gray, pose);
        }
        self.graph.search(&self.matcher, &self.key_mask, OVERLAP_SEARCH_PER_FRAME);
        self.prev_gray = Some(gray);
        StepInfo {
            pose: self.pose,
            confidence,
            speed,
            too_fast,
            lost,
            laser_only,
            detached: self.detached(),
            ..Default::default()
        }
    }

    /// Final image: the largest fragment, aligned globally and rendered again
    /// from its keyframes, cropped to the scanned area, with automatic levels.
    /// Also gives the part of the image without the irregular unscanned
    /// border (see [`scanned_area`]).
    pub fn finish_with_area(&self, auto_levels: bool) -> Option<(RgbImage, Option<[f32; 4]>)> {
        if !self.parked.is_empty() {
            log::warn!("{} fragment(s) non raccordé(s) au scan", self.parked.len());
        }
        let (live, graph) = std::iter::once((&self.canvas, &self.graph))
            .chain(self.parked.iter().map(|f| (&f.canvas, &f.graph)))
            .max_by_key(|(c, _)| c.covered_area())?;
        let aligned = (!graph.is_empty()).then(|| {
            let t0 = std::time::Instant::now();
            let poses = graph.solve(&self.matcher, &self.key_mask);
            let t1 = std::time::Instant::now();
            let canvas = graph.render(&poses, &self.weights);
            log::debug!("alignement {:.2} s, rendu {:.2} s", (t1 - t0).as_secs_f32(), t1.elapsed().as_secs_f32());
            canvas
        });
        let canvas = aligned.as_ref().unwrap_or(live);
        let mut img = canvas.render_rgb([255, 255, 255])?;
        let q = self.settings.output_quarter_turns;
        let mut area = None;
        if let Some((_, mask)) = canvas.coverage()
            && mask.len() == (img.width() * img.height()) as usize
        {
            if auto_levels {
                auto_white_balance(&mut img, &mask);
            }
            let mask =
                image::GrayImage::from_vec(img.width(), img.height(), mask.iter().map(|&m| m as u8 * 255).collect());
            area = mask.and_then(|m| scanned_area(&orient_image(q, &m)));
        }
        Some((orient_image(q, &img), area))
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

    /// Put down on a part of the page not scanned yet: scanning goes on
    /// separately, and the two parts are joined once the mouse comes back.
    #[test]
    fn landing_on_new_area_is_joined_later() {
        let doc = document(2000, 1600);
        let mut st = Stitcher::new(StitchSettings::default());
        let start = Pose { x: 500.0, y: 300.0, theta: 0.0 };
        let mut truth = start;
        let on = mouse(140);
        for _ in 0..40 {
            st.push_image(&view(&doc, truth), Some(&on));
            truth = truth.moved_local(12.0, 0.0, 0.0);
        }
        let dark = RgbImage::from_pixel(STITCH_FRAME.0, STITCH_FRAME.1, Rgb([20, 25, 15]));
        for _ in 0..6 {
            assert!(st.push_image(&dark, Some(&mouse(5))).lifted || st.frame_count() < 43);
        }
        // Well below the scanned strip, slightly rotated: nothing to find there.
        let mut truth = Pose { x: 700.0, y: 1000.0, theta: 0.1 };
        let mut detached = false;
        let mut joined_at = None;
        for i in 0..80 {
            let info = st.push_image(&view(&doc, truth), Some(&on));
            detached |= info.detached;
            if detached && !info.detached && !info.searching && joined_at.is_none() {
                joined_at = Some(i);
            }
            // Walk back up towards the first strip.
            let (s, c) = truth.theta.sin_cos();
            truth = Pose { x: truth.x + s * 10.0, y: truth.y - c * 10.0, ..truth };
        }
        assert!(detached, "the landing place was not reported as unknown");
        assert!(joined_at.is_some(), "the parts were never joined");
        assert!(st.parked.is_empty());
        // The pose is back in the coordinates of the first frame.
        let pose = st.pose();
        let (ex, ey) = (truth.x - start.x, truth.y - start.y);
        // `truth` was advanced once more after the last frame.
        let (s, c) = truth.theta.sin_cos();
        let (ex, ey) = (ex - s * 10.0, ey + c * 10.0);
        assert!((pose.x - ex).abs() < 8.0 && (pose.y - ey).abs() < 8.0, "pose {pose:?}, expected ({ex}, {ey})");
        assert!((pose.theta - truth.theta).abs() < 0.02, "theta {}", pose.theta);
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
