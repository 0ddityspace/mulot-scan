//! Motion measured by the two laser sensors of the mouse.
//!
//! The firmware reports, with every frame, the cumulative counts of two
//! optical (laser) sensors: words 0/1 for sensor A and 3/4 for sensor B, each
//! pair being the sensor's displacement along the frame's v and u axes. A
//! sensor at position `(px, py)` from the frame centre (in frame pixels) moves
//! by `(du - py·dθ, dv + px·dθ)` when the frame moves by `(du, dv)` and turns
//! by `dθ`, so the two sensors together give the full rigid motion — also on
//! blank paper, where the image alone cannot.
//!
//! The constants were fitted on recordings of real mice by comparing the
//! counts with the motion found by image registration (R² 0.95–0.99). They
//! are expressed in the stitcher's frame axes, i.e. after the mirror flip and
//! the stretch to square pixels (with it, each sensor has the same counts per
//! pixel along u and v, within 0.5 %).

use crate::device::MouseData;

struct Sensor {
    /// Indices of the u and v counters in [`MouseData::words`].
    u: usize,
    v: usize,
    /// Counts per frame pixel along u and v.
    su: f64,
    sv: f64,
    /// Position relative to the frame centre, in frame pixels.
    px: f64,
    py: f64,
}

const SENSORS: [Sensor; 2] = [
    Sensor { u: 1, v: 0, su: 3.457, sv: 3.474, px: 287.0, py: 258.0 },
    Sensor { u: 4, v: 3, su: 3.280, sv: 3.287, px: -488.0, py: 18.0 },
];

/// Below this surface quality a sensor's counts are ignored (on glossy or
/// metallic surfaces the counters stop moving).
const MIN_QUALITY: u8 = 40;
/// Larger jumps between two frames (~300 px) are glitches, not motion.
const MAX_COUNTS: i16 = 1000;

/// Turns successive counter readings into frame motion.
#[derive(Default)]
pub(super) struct LaserOdometry {
    last: Option<[u16; 6]>,
}

impl LaserOdometry {
    /// Forgets the previous reading (after a lift-off: counts made in the air are meaningless).
    pub(super) fn reset(&mut self) {
        self.last = None;
    }

    /// Motion `(du, dv, dθ)` of the frame since the previous reading, in the
    /// stitcher's frame axes. `mirrored` tells whether frames are flipped
    /// vertically before stitching (the constants assume they are).
    pub(super) fn update(&mut self, d: &MouseData, mirrored: bool) -> Option<(f64, f64, f64)> {
        let last = self.last.replace(d.words)?;
        let delta = |i: usize| d.words[i].wrapping_sub(last[i]) as i16;
        // Each valid sensor gives two equations in (du, dv, dθ).
        let mut rows: Vec<([f64; 3], f64)> = Vec::with_capacity(4);
        for (s, &q) in SENSORS.iter().zip(&d.surface_quality) {
            let (cu, cv) = (delta(s.u), delta(s.v));
            if q < MIN_QUALITY || cu.abs() > MAX_COUNTS || cv.abs() > MAX_COUNTS {
                continue;
            }
            rows.push(([1.0, 0.0, -s.py], cu as f64 / s.su));
            rows.push(([0.0, 1.0, s.px], cv as f64 / s.sv));
        }
        let (du, dv, dtheta) = match rows.len() {
            // One sensor: translation only.
            2 => (rows[0].1, rows[1].1, 0.0),
            4 => least_squares(&rows)?,
            _ => return None,
        };
        // A vertical flip negates v and the direction of rotation.
        Some(if mirrored { (du, dv, dtheta) } else { (du, -dv, -dtheta) })
    }
}

/// Least-squares solution of `a · x = b` for three unknowns.
fn least_squares(rows: &[([f64; 3], f64)]) -> Option<(f64, f64, f64)> {
    let mut ata = [[0f64; 3]; 3];
    let mut atb = [0f64; 3];
    for (a, b) in rows {
        for r in 0..3 {
            atb[r] += a[r] * b;
            for c in 0..3 {
                ata[r][c] += a[r] * a[c];
            }
        }
    }
    let x = super::linalg::solve(ata, atb)?;
    Some((x[0], x[1], x[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counter readings after a frame motion, as the firmware would report them.
    fn counts(words: &mut [u16; 6], du: f64, dv: f64, dtheta: f64) {
        for s in &SENSORS {
            let cu = ((du - s.py * dtheta) * s.su).round() as i16;
            let cv = ((dv + s.px * dtheta) * s.sv).round() as i16;
            words[s.u] = words[s.u].wrapping_add(cu as u16);
            words[s.v] = words[s.v].wrapping_add(cv as u16);
        }
    }

    fn data(words: [u16; 6], q: u8) -> MouseData {
        MouseData { words, surface_quality: [q, q], ..Default::default() }
    }

    #[test]
    fn recovers_translation_and_rotation_across_counter_wrap() {
        let mut odo = LaserOdometry::default();
        let mut words = [65500, 65530, 0, 10, 65535, 0];
        assert_eq!(odo.update(&data(words, 150), true), None);
        counts(&mut words, 12.0, -7.0, 0.02);
        let (du, dv, dt) = odo.update(&data(words, 150), true).unwrap();
        assert!((du - 12.0).abs() < 0.5 && (dv + 7.0).abs() < 0.5, "({du}, {dv})");
        assert!((dt - 0.02).abs() < 0.002, "{dt}");
        // Not mirrored: v and the rotation change sign.
        counts(&mut words, 12.0, -7.0, 0.02);
        let (_, dv, dt) = odo.update(&data(words, 150), false).unwrap();
        assert!((dv - 7.0).abs() < 0.5 && (dt + 0.02).abs() < 0.002);
    }

    #[test]
    fn ignores_sensors_that_lost_the_surface() {
        let mut odo = LaserOdometry::default();
        let mut words = [0; 6];
        odo.update(&data(words, 150), true);
        counts(&mut words, 5.0, 5.0, 0.0);
        assert_eq!(odo.update(&data(words, 10), true), None);
        // One good sensor: translation only.
        counts(&mut words, 5.0, 5.0, 0.0);
        let mut d = data(words, 150);
        d.surface_quality[1] = 10;
        let (du, dv, dt) = odo.update(&d, true).unwrap();
        assert!((du - 5.0).abs() < 1.5 && (dv - 5.0).abs() < 0.5 && dt == 0.0, "({du}, {dv}, {dt})");
    }
}
