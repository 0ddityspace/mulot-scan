//! Translation estimation by phase correlation.

use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

/// A single-channel f32 image.
#[derive(Clone, Debug)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub px: Vec<f32>,
}

impl Gray {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, px: vec![0.0; w * h] }
    }

    pub fn crop(&self, x0: usize, y0: usize, w: usize, h: usize) -> Gray {
        let mut out = Gray::new(w, h);
        for y in 0..h {
            let src = &self.px[(y0 + y) * self.w + x0..(y0 + y) * self.w + x0 + w];
            out.px[y * w..(y + 1) * w].copy_from_slice(src);
        }
        out
    }
}

/// Result of a phase correlation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Shift {
    /// Displacement such that `cur(x) ≈ reference(x + d)`.
    pub dx: f32,
    pub dy: f32,
    /// Peak sharpness (peak / mean of the correlation surface). Values above ~8 are reliable.
    pub confidence: f32,
}

/// Cached FFT plans and windows for a fixed image size.
pub struct PhaseCorrelator {
    w: usize,
    h: usize,
    row_fwd: Arc<dyn Fft<f32>>,
    col_fwd: Arc<dyn Fft<f32>>,
    row_inv: Arc<dyn Fft<f32>>,
    col_inv: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    scratch: Vec<Complex32>,
    col: Vec<Complex32>,
}

impl PhaseCorrelator {
    pub fn new(w: usize, h: usize) -> Self {
        let mut p = FftPlanner::new();
        let hann = |n: usize, i: usize| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * (i as f32 + 0.5) / n as f32).cos();
        let mut window = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                window[y * w + x] = hann(w, x) * hann(h, y);
            }
        }
        Self {
            w,
            h,
            row_fwd: p.plan_fft_forward(w),
            col_fwd: p.plan_fft_forward(h),
            row_inv: p.plan_fft_inverse(w),
            col_inv: p.plan_fft_inverse(h),
            window,
            scratch: Vec::new(),
            col: vec![Complex32::default(); h],
        }
    }

    fn fft2(&mut self, data: &mut [Complex32], inverse: bool) {
        let (w, h) = (self.w, self.h);
        let (row, col) = if inverse {
            (self.row_inv.clone(), self.col_inv.clone())
        } else {
            (self.row_fwd.clone(), self.col_fwd.clone())
        };
        let need = row.get_inplace_scratch_len().max(col.get_inplace_scratch_len());
        if self.scratch.len() < need {
            self.scratch.resize(need, Complex32::default());
        }
        for r in data.chunks_exact_mut(w) {
            row.process_with_scratch(r, &mut self.scratch[..row.get_inplace_scratch_len()]);
        }
        for x in 0..w {
            for y in 0..h {
                self.col[y] = data[y * w + x];
            }
            col.process_with_scratch(&mut self.col, &mut self.scratch[..col.get_inplace_scratch_len()]);
            for y in 0..h {
                data[y * w + x] = self.col[y];
            }
        }
    }

    fn spectrum(&mut self, img: &Gray) -> Vec<Complex32> {
        debug_assert_eq!((img.w, img.h), (self.w, self.h));
        let n = img.px.len() as f32;
        let mean = img.px.iter().sum::<f32>() / n;
        let mut data: Vec<Complex32> =
            img.px.iter().zip(&self.window).map(|(&v, &win)| Complex32::new((v - mean) * win, 0.0)).collect();
        self.fft2(&mut data, false);
        data
    }

    /// Estimates the translation between `reference` and `current`.
    pub fn correlate(&mut self, reference: &Gray, current: &Gray) -> Shift {
        let a = self.spectrum(reference);
        let b = self.spectrum(current);
        let mut r: Vec<Complex32> = a
            .iter()
            .zip(&b)
            .map(|(x, y)| {
                let c = y * x.conj();
                let m = c.norm();
                if m > 1e-12 { c / m } else { Complex32::default() }
            })
            .collect();
        self.fft2(&mut r, true);
        let (w, h) = (self.w, self.h);
        let mut best = (0usize, f32::MIN);
        let mut sum = 0.0f32;
        for (i, v) in r.iter().enumerate() {
            let re = v.re;
            sum += re.abs();
            if re > best.1 {
                best = (i, re);
            }
        }
        let mean = sum / (w * h) as f32;
        let (bx, by) = (best.0 % w, best.0 / w);
        let val = |x: isize, y: isize| -> f32 {
            let xx = x.rem_euclid(w as isize) as usize;
            let yy = y.rem_euclid(h as isize) as usize;
            r[yy * w + xx].re
        };
        // Sub-pixel refinement with a parabolic fit.
        let sub = |m: f32, c: f32, p: f32| -> f32 {
            let d = m - 2.0 * c + p;
            if d.abs() < 1e-12 { 0.0 } else { 0.5 * (m - p) / d }
        };
        let (bxi, byi) = (bx as isize, by as isize);
        let c = val(bxi, byi);
        let ox = sub(val(bxi - 1, byi), c, val(bxi + 1, byi));
        let oy = sub(val(bxi, byi - 1), c, val(bxi, byi + 1));
        let mut dx = bx as f32 + ox;
        let mut dy = by as f32 + oy;
        if dx > w as f32 / 2.0 {
            dx -= w as f32;
        }
        if dy > h as f32 / 2.0 {
            dy -= h as f32;
        }
        Shift { dx: -dx, dy: -dy, confidence: if mean > 0.0 { best.1 / mean } else { 0.0 } }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(w: usize, h: usize, ox: f32, oy: f32) -> Gray {
        let mut g = Gray::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let fx = x as f32 + ox;
                let fy = y as f32 + oy;
                g.px[y * w + x] = (fx * 0.31).sin() * (fy * 0.17).cos()
                    + ((fx * 0.07 + fy * 0.11).sin() * 3.0).sin()
                    + ((fx * 0.023).cos() * (fy * 0.041).sin());
            }
        }
        g
    }

    #[test]
    fn recovers_translation() {
        let (w, h) = (128, 64);
        let reference = pattern(w, h, 0.0, 0.0);
        // current(x) = reference(x + d)
        let current = pattern(w, h, 7.0, -4.0);
        let mut pc = PhaseCorrelator::new(w, h);
        let s = pc.correlate(&reference, &current);
        assert!((s.dx - 7.0).abs() < 0.3, "{s:?}");
        assert!((s.dy + 4.0).abs() < 0.3, "{s:?}");
        assert!(s.confidence > 8.0, "{s:?}");
    }
}
