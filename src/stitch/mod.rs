//! Real-time mosaicking of the frames captured while the mouse moves over a
//! document.
//!
//! Each frame is registered against the mosaic built so far (phase
//! correlation on two half-images gives translation and rotation), with a
//! constant-velocity / optical-sensor prediction used as a starting point and
//! as a fallback on featureless areas.  Registered frames are blended into a
//! tiled canvas with centre-weighted feathering.

mod canvas;
mod flat;
mod laser;
mod lift;
mod linalg;
mod output;
mod register;
mod stitcher;

use serde::{Deserialize, Serialize};

pub use canvas::Bounds;
pub use flat::FlatField;
pub use output::{orient_image, orient_point};
pub use stitcher::{STITCH_FRAME, StepInfo, Stitcher};

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
    /// Use the motion measured by the mouse's laser sensors as prediction,
    /// and to keep tracking on blank areas.
    pub use_laser: bool,
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
            use_laser: true,
            auto_flat: true,
            lift_sensitivity: 50,
            output_quarter_turns: 3,
        }
    }
}
