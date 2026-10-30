//! Lift-off detection.

/// Lift-off detection from the optical sensors and the image brightness.
#[derive(Default)]
pub(super) struct LiftDetector {
    reference: f32,
    lifted: bool,
    bad: u32,
    good: u32,
}

impl LiftDetector {
    pub(super) fn update(&mut self, brightness: f32, quality: Option<[u8; 2]>, threshold: u8) -> bool {
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
