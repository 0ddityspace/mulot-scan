//! Lift-off detection.
//!
//! On real recordings a lift-off looks like this: the mouse tilts first, so
//! one laser sensor loses the surface two or three frames before the other
//! while the image gets darker and blurred; then both sensors report a
//! quality near 0. Landing is the same in reverse, and the image stays dark
//! and blurred until both sensors see the paper again.

/// Brightness ratio (to the running reference) below which a frame counts as
/// darkened. Alone it is not a lift-off (dark pictures on the page), but it
/// confirms a sensor that lost the surface.
const DIMMED: f32 = 0.92;
/// Brightness ratio that means lift-off even without sensor data.
const DARK: f32 = 0.5;
/// Consecutive bad frames before a lift-off is declared.
const LIFT_FRAMES: u32 = 2;
/// Consecutive good frames before the mouse is considered back on the page.
const LAND_FRAMES: u32 = 3;
/// Consecutive frames that are not bad, though not good either (one sensor
/// on a glossy surface), after which the mouse is back on the page anyway.
const LAND_FRAMES_DEGRADED: u32 = 10;

/// Lift-off detection from the optical sensors and the image brightness.
#[derive(Default)]
pub(super) struct LiftDetector {
    reference: f32,
    lifted: bool,
    bad: u32,
    good: u32,
    not_bad: u32,
    samples: u32,
}

impl LiftDetector {
    /// Updates the state with one frame; returns whether the mouse is lifted.
    /// `threshold` is the sensors' surface quality threshold (0: sensors ignored).
    pub(super) fn update(&mut self, brightness: f32, quality: Option<[u8; 2]>, threshold: u8) -> bool {
        let quality = quality.filter(|_| threshold > 0);
        let low = quality.map_or(0, |q| q.iter().filter(|&&v| v < threshold).count());
        let ratio = if self.reference > 0.0 { brightness / self.reference } else { 1.0 };
        let bad = low == 2 || (low == 1 && ratio < DIMMED) || ratio < DARK;
        // Back on the page: both sensors see the surface and the image is bright
        // again (without sensors, brightness alone).
        let good = low == 0 && ratio >= DIMMED;
        if bad {
            self.bad += 1;
            self.good = 0;
            self.not_bad = 0;
        } else {
            self.bad = 0;
            self.not_bad += 1;
            self.good = if good { self.good + 1 } else { 0 };
            if !self.lifted {
                // Running mean, fast at first (the exposure settles during the
                // first frames), then slow.
                self.samples += 1;
                let a = (1.0 / self.samples as f32).max(0.05);
                self.reference += (brightness - self.reference) * a;
            }
        }
        if !self.lifted && self.bad >= LIFT_FRAMES {
            self.lifted = true;
        } else if self.lifted && (self.good >= LAND_FRAMES || self.not_bad >= LAND_FRAMES_DEGRADED) {
            self.lifted = false;
        }
        self.lifted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames 62–95 of a real recording (scan-007): brightness, sensor qualities.
    const LIFT: [(f32, [u8; 2]); 22] = [
        (154.6, [137, 140]),
        (154.8, [141, 143]),
        (150.5, [130, 142]),
        (137.7, [30, 131]),
        (120.1, [3, 142]),
        (101.5, [0, 30]),
        (88.2, [0, 0]),
        (75.6, [0, 0]),
        (79.6, [0, 0]),
        (98.2, [1, 1]),
        (107.3, [3, 11]),
        (119.5, [10, 27]),
        (132.6, [35, 136]),
        (137.6, [81, 139]),
        (140.7, [128, 150]),
        (143.9, [137, 126]),
        (147.3, [150, 140]),
        (147.1, [135, 152]),
        (148.3, [128, 129]),
        (148.5, [135, 135]),
        (149.2, [136, 134]),
        (149.1, [153, 160]),
    ];

    #[test]
    fn detects_tilt_early_and_waits_for_a_steady_landing() {
        let mut d = LiftDetector::default();
        // The first frame is brighter while the exposure settles.
        assert!(!d.update(218.0, Some([140, 140]), 50));
        for _ in 0..60 {
            assert!(!d.update(154.0, Some([140, 140]), 50));
        }
        let states: Vec<bool> = LIFT.iter().map(|&(b, q)| d.update(b, Some(q), 50)).collect();
        // Lifted on the second tilted frame, not when both sensors are lost.
        assert_eq!(states.iter().position(|&l| l), Some(4));
        // Down again once both sensors and the brightness have recovered.
        let landed = states.iter().rposition(|&l| l).unwrap() + 1;
        assert!(landed >= 17, "landed at {landed}");
        assert!(!states[landed..].iter().any(|&l| l));
    }

    #[test]
    fn one_sensor_on_glossy_paper_is_not_a_lift() {
        let mut d = LiftDetector::default();
        for i in 0..60 {
            let q = if i > 20 { [10, 140] } else { [140, 140] };
            assert!(!d.update(150.0, Some(q), 50), "frame {i}");
        }
    }

    #[test]
    fn lands_on_glossy_paper_eventually() {
        let mut d = LiftDetector::default();
        for _ in 0..30 {
            d.update(150.0, Some([140, 140]), 50);
        }
        assert!(d.update(60.0, Some([0, 0]), 50) | d.update(60.0, Some([0, 0]), 50));
        let landed = (0..20).position(|_| !d.update(150.0, Some([10, 140]), 50));
        assert_eq!(landed, Some(LAND_FRAMES_DEGRADED as usize - 1));
    }
}
