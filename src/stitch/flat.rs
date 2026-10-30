//! Illumination correction: calibrated flat field and self-calibrating model.

use image::RgbImage;
use serde::{Deserialize, Serialize};

use super::canvas::to_u8;
use super::linalg::solve;

/// Per-pixel flat-field correction (vignetting + colour cast).
#[derive(Clone, Serialize, Deserialize)]
pub struct FlatField {
    pub width: u32,
    pub height: u32,
    /// RGB gains, row-major.
    pub gains: Vec<[f32; 3]>,
}

impl FlatField {
    /// Builds a flat field from frames of a white sheet.
    ///
    /// The illumination of the scan window varies smoothly, so anything sharper
    /// (printed text, show-through from the back of the page, dust) is rejected
    /// instead of being baked into the correction, where it would leave bright
    /// marks on every frame and attract the registration.
    pub fn from_white_frames(frames: &[&RgbImage]) -> Result<Self, CalibrationError> {
        let first = frames.first().ok_or(CalibrationError::NoFrames)?;
        let (w, h) = first.dimensions();
        let n = (w * h) as usize;
        let mut acc = vec![[0f32; 3]; n];
        let mut cnt = 0.0f32;
        for f in frames.iter().filter(|f| f.dimensions() == (w, h)) {
            for (a, p) in acc.iter_mut().zip(f.pixels()) {
                for k in 0..3 {
                    a[k] += p.0[k] as f32;
                }
            }
            cnt += 1.0;
        }
        for a in &mut acc {
            for v in a.iter_mut() {
                *v /= cnt;
            }
        }
        let response = smooth_response(&acc, w as usize, h as usize)?;
        Ok(Self::from_response(&response, w, h))
    }

    /// Recomputes the gains through the robust illumination model, removing any
    /// texture of the calibration sheet left in an older calibration.
    pub fn sanitized(self) -> Result<Self, CalibrationError> {
        let (w, h) = (self.width, self.height);
        let response: Vec<[f32; 3]> = self.gains.iter().map(|g| g.map(|v| 1000.0 / v.max(1e-3))).collect();
        Ok(Self::from_response(&smooth_response(&response, w as usize, h as usize)?, w, h))
    }

    /// Gains that bring `response` (the image of a white sheet) to a uniform white.
    fn from_response(response: &[[f32; 3]], w: u32, h: u32) -> Self {
        // Target: the brightest (98th percentile) response of each channel.
        let mut target = [0f32; 3];
        for (k, t) in target.iter_mut().enumerate() {
            let mut v: Vec<f32> = response.iter().map(|p| p[k]).collect();
            v.sort_by(|a, b| a.total_cmp(b));
            *t = v[(v.len() as f32 * 0.98) as usize].max(1.0);
        }
        let t = target.iter().copied().fold(0.0, f32::max);
        let gains = response.iter().map(|p| [0, 1, 2].map(|k| (t / p[k].max(4.0)).min(8.0) * 0.96)).collect();
        Self { width: w, height: h, gains }
    }

    pub fn apply(&self, img: &RgbImage) -> RgbImage {
        let mut out = img.clone();
        if img.dimensions() != (self.width, self.height) {
            return out;
        }
        for (p, g) in out.pixels_mut().zip(&self.gains) {
            for k in 0..3 {
                p.0[k] = to_u8(p.0[k] as f32 * g[k]);
            }
        }
        out
    }
}

/// Why a calibration was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum CalibrationError {
    NoFrames,
    /// Share of the sheet that is darker than the illumination model.
    NotUniform(f32),
}

impl std::fmt::Display for CalibrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoFrames => write!(f, "aucune image reçue"),
            Self::NotUniform(share) => write!(
                f,
                "la feuille n'est pas uniforme ({:.0} % de zones plus sombres : texte, verso visible par \
                 transparence ou taches). Recommencez sur une feuille blanche vierge et épaisse.",
                share * 100.0
            ),
        }
    }
}

impl std::error::Error for CalibrationError {}

/// Cell size (pixels) of the illumination model.
const CELL: usize = 8;
/// Cells darker than this fraction of the smooth model are not paper.
const DARK_OUTLIER: f64 = 0.93;
/// Above this share of dark cells the sheet is refused.
const MAX_DARK_SHARE: f32 = 0.2;

/// Smooth illumination response from the mean image of a white sheet.
///
/// The image is reduced to 8×8-pixel cells (median per channel). A degree-4
/// polynomial surface is fitted to the cell luminance while iteratively
/// discarding cells that are clearly darker than the fit. Kept cells keep
/// their measured value (fine vignetting detail); discarded ones take the fit.
fn smooth_response(mean: &[[f32; 3]], w: usize, h: usize) -> Result<Vec<[f32; 3]>, CalibrationError> {
    let (gw, gh) = (w.div_ceil(CELL), h.div_ceil(CELL));
    let mut cells = vec![[0f32; 3]; gw * gh];
    let mut vals = Vec::with_capacity(CELL * CELL);
    for gy in 0..gh {
        for gx in 0..gw {
            for k in 0..3 {
                vals.clear();
                for y in gy * CELL..((gy + 1) * CELL).min(h) {
                    vals.extend(mean[y * w + gx * CELL..y * w + ((gx + 1) * CELL).min(w)].iter().map(|p| p[k]));
                }
                let mid = vals.len() / 2;
                cells[gy * gw + gx][k] = *vals.select_nth_unstable_by(mid, f32::total_cmp).1;
            }
        }
    }
    let luma = |c: &[f32; 3]| (0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]) as f64;

    let mut good = vec![true; gw * gh];
    let mut fit = [0f64; POLY_TERMS];
    for _ in 0..5 {
        fit = fit_poly(&cells, &good, gw, gh, &luma).ok_or(CalibrationError::NotUniform(1.0))?;
        for (i, (c, g)) in cells.iter().zip(&mut good).enumerate() {
            let (x, y) = cell_coord(i, gw, gh);
            *g = luma(c) >= DARK_OUTLIER * eval_poly(&fit, x, y);
        }
    }
    // Judge the sheet away from the window border, which is dark anyway.
    let interior: Vec<bool> = (0..gw * gh)
        .filter(|i| (2..gw - 2).contains(&(i % gw)) && (2..gh - 2).contains(&(i / gw)))
        .map(|i| good[i])
        .collect();
    let dark_share = interior.iter().filter(|g| !**g).count() as f32 / interior.len().max(1) as f32;
    if dark_share > MAX_DARK_SHARE {
        return Err(CalibrationError::NotUniform(dark_share));
    }
    // Replace rejected cells by the fitted surface, per channel.
    for k in 0..3 {
        let fit_k = fit_poly(&cells, &good, gw, gh, &|c: &[f32; 3]| c[k] as f64).unwrap_or(fit);
        for (i, c) in cells.iter_mut().enumerate() {
            if !good[i] {
                let (x, y) = cell_coord(i, gw, gh);
                c[k] = eval_poly(&fit_k, x, y) as f32;
            }
        }
    }
    // Bilinear interpolation between cell centres, then a light blur to hide the grid.
    let mut out = vec![[0f32; 3]; w * h];
    for y in 0..h {
        let fy = ((y as f32 + 0.5) / CELL as f32 - 0.5).clamp(0.0, (gh - 1) as f32);
        let (y0, ay) = (fy as usize, fy.fract());
        let y1 = (y0 + 1).min(gh - 1);
        for x in 0..w {
            let fx = ((x as f32 + 0.5) / CELL as f32 - 0.5).clamp(0.0, (gw - 1) as f32);
            let (x0, ax) = (fx as usize, fx.fract());
            let x1 = (x0 + 1).min(gw - 1);
            let c = |gx: usize, gy: usize| cells[gy * gw + gx];
            out[y * w + x] = std::array::from_fn(|k| {
                c(x0, y0)[k] * (1.0 - ax) * (1.0 - ay)
                    + c(x1, y0)[k] * ax * (1.0 - ay)
                    + c(x0, y1)[k] * (1.0 - ax) * ay
                    + c(x1, y1)[k] * ax * ay
            });
        }
    }
    Ok(box_blur3(&out, w, h, 3))
}

/// Centre of cell `i` in normalised coordinates (-0.5..0.5).
fn cell_coord(i: usize, gw: usize, gh: usize) -> (f64, f64) {
    (((i % gw) as f64 + 0.5) / gw as f64 - 0.5, ((i / gw) as f64 + 0.5) / gh as f64 - 0.5)
}

/// Number of monomials x^i·y^j with i + j <= 4.
const POLY_TERMS: usize = 15;

fn poly_basis(x: f64, y: f64) -> [f64; POLY_TERMS] {
    let mut b = [0f64; POLY_TERMS];
    let mut n = 0;
    for d in 0..=4 {
        for j in 0..=d {
            b[n] = x.powi(d - j) * y.powi(j);
            n += 1;
        }
    }
    b
}

fn eval_poly(c: &[f64; POLY_TERMS], x: f64, y: f64) -> f64 {
    c.iter().zip(poly_basis(x, y)).map(|(c, b)| c * b).sum()
}

/// Least-squares fit of `value(cell)` over the `good` cells.
fn fit_poly(
    cells: &[[f32; 3]],
    good: &[bool],
    gw: usize,
    gh: usize,
    value: &dyn Fn(&[f32; 3]) -> f64,
) -> Option<[f64; POLY_TERMS]> {
    let mut ata = [[0f64; POLY_TERMS]; POLY_TERMS];
    let mut atb = [0f64; POLY_TERMS];
    let mut n = 0;
    for (i, c) in cells.iter().enumerate().filter(|(i, _)| good[*i]) {
        let (x, y) = cell_coord(i, gw, gh);
        let b = poly_basis(x, y);
        let v = value(c);
        for r in 0..POLY_TERMS {
            atb[r] += b[r] * v;
            for col in 0..POLY_TERMS {
                ata[r][col] += b[r] * b[col];
            }
        }
        n += 1;
    }
    if n < POLY_TERMS * 4 {
        return None;
    }
    for (r, row) in ata.iter_mut().enumerate() {
        row[r] += 1e-9;
    }
    solve(ata, atb)
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
        let iw = w as usize;
        let raw = img.as_raw();
        let mut ch = vec![0u8; (c * c) as usize];
        for gy in 0..self.gh {
            for gx in 0..self.gw {
                let (x0, y0) = (gx as u32 * c, gy as u32 * c);
                // Skip the dark border/corners of the window.
                let corner = (x0 < 32 || x0 + c > w - 32) && (y0 < 32 || y0 + c > h - 32);
                if x0 < margin || y0 < margin || x0 + c > w - margin || y0 + c > h - margin || corner {
                    continue;
                }
                let i = gy * self.gw + gx;
                for k in 0..3 {
                    for (dst, y) in ch.chunks_exact_mut(c as usize).zip(y0 as usize..) {
                        let row = &raw[(y * iw + x0 as usize) * 3..][..c as usize * 3];
                        for (d, p) in dst.iter_mut().zip(row.as_chunks::<3>().0) {
                            *d = p[k];
                        }
                    }
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
            match solve(ata, atb) {
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
        let target = self.peak.iter().copied().fold(0.0f32, f32::max);
        let mut out = img.clone();
        for (y, row) in out.chunks_exact_mut(w as usize * 3).enumerate() {
            let fy = (y as f64 + 0.5) / h as f64 - 0.5;
            // Along a row the model is a quadratic in x: a + b·x + q·x².
            let poly =
                coeffs.map(|c| [(c[0] + c[2] * fy + c[4] * fy * fy) as f32, (c[1] + c[5] * fy) as f32, c[3] as f32]);
            for (x, p) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                let fx = (x as f32 + 0.5) / w as f32 - 0.5;
                for k in 0..3 {
                    let [a, b, q] = poly[k];
                    let model = a + fx * (b + fx * q);
                    let gain = (target / model.max(8.0)).clamp(0.5, 3.0) * 0.97;
                    p[k] = to_u8(p[k] as f32 * gain);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    /// White sheet seen through the scan window: vignetting and a green cast.
    fn sheet(x: u32, y: u32) -> [f32; 3] {
        let (u, v) = (x as f32 / 640.0 - 0.5, y as f32 / 296.0 - 0.5);
        let vig = 1.0 - 0.8 * (u * u + v * v);
        [150.0 * vig, 200.0 * vig, 90.0 * vig]
    }

    /// Frames of `sheet` with dark strokes wherever `ink(x, y)` is true.
    fn frames(ink: impl Fn(u32, u32) -> bool) -> Vec<RgbImage> {
        let img = RgbImage::from_fn(640, 296, |x, y| {
            let s = sheet(x, y);
            Rgb(if ink(x, y) { [30, 40, 20] } else { s.map(|v| v as u8) })
        });
        vec![img; 4]
    }

    fn calibrate(frames: &[RgbImage]) -> Result<FlatField, CalibrationError> {
        FlatField::from_white_frames(&frames.iter().collect::<Vec<_>>())
    }

    #[test]
    fn text_on_the_sheet_is_not_baked_into_the_correction() {
        // A few lines of 2-pixel strokes, like a word or show-through.
        let ink =
            |x: u32, y: u32| (200..440).contains(&x) && (100..130).contains(&y) && (x / 3 + y / 7).is_multiple_of(4);
        let ff = calibrate(&frames(ink)).expect("lightly printed sheet accepted");
        let gain = |x: u32, y: u32| ff.gains[(y * 640 + x) as usize][1];
        // Inside the printed area the gain follows the vignetting, not the ink.
        for (x, y) in [(300, 115), (301, 115), (350, 110), (400, 125)] {
            let expected = gain(300, 160) * sheet(300, 160)[1] / sheet(x, y)[1];
            assert!((gain(x, y) / expected - 1.0).abs() < 0.05, "gain {} vs {expected} at ({x}, {y})", gain(x, y));
        }
        // Vignetting is still corrected: the corrected sheet is flat.
        let flat = |x: u32, y: u32| sheet(x, y)[1] * gain(x, y);
        assert!((flat(60, 40) / flat(320, 148) - 1.0).abs() < 0.05);
    }

    #[test]
    fn sanitizing_a_clean_calibration_changes_little() {
        let ff = calibrate(&frames(|_, _| false)).unwrap();
        let again = ff.clone().sanitized().unwrap();
        // Away from the dark rim of the window.
        for y in (20..276).step_by(16) {
            for x in (20..620).step_by(16) {
                let (a, b) = (ff.gains[y * 640 + x], again.gains[y * 640 + x]);
                assert!(a.iter().zip(b).all(|(a, b)| (a / b - 1.0).abs() < 0.03), "({x}, {y}): {a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn printed_page_is_refused() {
        // Dense bold print (or a photo) over most of the window.
        let ink = |x: u32, y: u32| !(y / 12).is_multiple_of(3) && !(x / 5).is_multiple_of(3);
        match calibrate(&frames(ink)) {
            Err(CalibrationError::NotUniform(share)) => assert!(share > MAX_DARK_SHARE),
            other => panic!("expected a refusal, got {:?}", other.map(|_| ())),
        }
    }
}
