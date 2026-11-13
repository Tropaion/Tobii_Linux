//! Response curves for the head-tracking output axes.
//!
//! Extended View has had a curve since it was written ([`AxisResponse`]); the
//! head pose itself went out linearly. opentrack and TrackIR both put a curve
//! stage in front of the wire because a linear mapping forces a choice nobody
//! wants: enough gain to look over your shoulder with a 20° head turn also
//! amplifies every tremor at the centre. A curve that starts flat and steepens
//! towards the edge gives both, and a small deadzone on top removes the last
//! of the centre shimmer.
//!
//! # The same mapping as Extended View
//!
//! Each axis is an [`AxisResponse`] — deadzone, the input that reaches full
//! output, that output, and the shape between — so a user who has tuned
//! `ev_yaw_*` already knows what `head_yaw_*` does. The `_deg` field names are
//! historical: for the three translation axes the same numbers are
//! millimetres.
//!
//! # Identity by default
//!
//! The defaults are linear, with no deadzone and input range equal to output
//! range, which is a pass-through. [`HeadCurves::apply`] recognises that and
//! returns the value bit-identically rather than round-tripping it through a
//! division, so nothing changes for anyone who has not asked for a curve.
//! The range sits at the physical limit of each quantity — ±180° yaw and roll,
//! ±90° pitch, ±500 mm translation, the same full scales the virtual joystick
//! uses — so the saturation an `AxisResponse` applies past `input_max` is
//! unreachable at the defaults.

use tobii_headpose::HeadPose;

use crate::fusion::{AxisResponse, Curve};

/// The six output axes, in the order [`HeadCurves::axes`] stores them and the
/// config file names them.
pub const AXIS_NAMES: [&str; 6] = ["yaw", "pitch", "roll", "x", "y", "z"];

/// Default full-scale range per axis: degrees for the rotations, millimetres
/// for the translations.
const FULL_SCALE: [f64; 6] = [180.0, 90.0, 180.0, 500.0, 500.0, 500.0];

/// Which axes leave mirrored by default: yaw, pitch, roll, x, y, z.
///
/// Only x. The tracker's x axis runs the other way round from the one opentrack,
/// FreeTrack and TrackIR games expect for sideways translation: leaning to the
/// right moved the in-game head to the left, measured in play. Rotation was
/// right as it was. Flipped here, on the way out, rather than in
/// `pose_from_eyes`, because Extended View needs the head's position in the
/// tracker's own frame to measure against the display corners.
pub const DEFAULT_INVERT: [bool; 6] = [false, false, false, true, false, false];

/// One curve per output axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeadCurves {
    /// Yaw, pitch, roll, x, y, z — see [`AXIS_NAMES`].
    pub axes: [AxisResponse; 6],
    /// Whether each axis is negated after shaping, same order as `axes`.
    pub invert: [bool; 6],
}

/// The pass-through response for an axis whose full scale is `range`.
fn identity(range: f64) -> AxisResponse {
    AxisResponse {
        deadzone_deg: 0.0,
        input_max_deg: range,
        output_max_deg: range,
        curve: Curve::Linear,
        // The mapping already saturates at `output_max`; no second ceiling.
        clamp_deg: f64::INFINITY,
    }
}

impl Default for HeadCurves {
    fn default() -> Self {
        HeadCurves {
            axes: FULL_SCALE.map(identity),
            invert: DEFAULT_INVERT,
        }
    }
}

/// Whether `r` maps every value inside its range to itself.
fn is_identity(r: &AxisResponse) -> bool {
    r.deadzone_deg <= 0.0
        && r.curve == Curve::Linear
        && (r.input_max_deg - r.output_max_deg).abs() <= f64::EPSILON * r.input_max_deg.abs()
}

impl HeadCurves {
    /// The axis named `name` in [`AXIS_NAMES`].
    pub fn axis_mut(&mut self, name: &str) -> Option<&mut AxisResponse> {
        let i = AXIS_NAMES.iter().position(|n| *n == name)?;
        Some(&mut self.axes[i])
    }

    /// The invert flag of the axis named `name` in [`AXIS_NAMES`].
    pub fn invert_mut(&mut self, name: &str) -> Option<&mut bool> {
        let i = AXIS_NAMES.iter().position(|n| *n == name)?;
        Some(&mut self.invert[i])
    }

    /// Shape every axis of `p`.
    ///
    /// Applied to the **finished** pose — after the neutral, the rotation
    /// reference, Extended View and the filter — so the deadzone measures
    /// distance from the centre the user actually sees, and so the filter has
    /// already removed the noise a deadzone would otherwise have to be wide
    /// enough to swallow.
    pub fn apply(&self, p: HeadPose) -> HeadPose {
        let shape = |i: usize, v: f64| {
            let r = &self.axes[i];
            let out = if is_identity(r) && v.abs() <= r.input_max_deg {
                v
            } else {
                r.shape(v)
            };
            if self.invert[i] {
                -out
            } else {
                out
            }
        };
        HeadPose {
            yaw_deg: shape(0, p.yaw_deg),
            pitch_deg: shape(1, p.pitch_deg),
            roll_deg: shape(2, p.roll_deg),
            x_mm: shape(3, p.x_mm),
            y_mm: shape(4, p.y_mm),
            z_mm: shape(5, p.z_mm),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose() -> HeadPose {
        HeadPose {
            x_mm: 12.345,
            y_mm: -7.5,
            z_mm: 33.3,
            yaw_deg: 17.123_456_789,
            pitch_deg: -4.2,
            roll_deg: 2.25,
        }
    }

    #[test]
    fn the_default_is_a_bit_identical_pass_through_apart_from_mirroring_x() {
        let mut want = pose();
        want.x_mm = -want.x_mm;
        assert_eq!(HeadCurves::default().apply(pose()), want);
        let plain = HeadCurves {
            invert: [false; 6],
            ..Default::default()
        };
        assert_eq!(plain.apply(pose()), pose());
    }

    #[test]
    fn a_deadzone_zeroes_small_movements_and_starts_smoothly_after_it() {
        let mut c = HeadCurves::default();
        c.axis_mut("yaw").unwrap().deadzone_deg = 3.0;
        let at = |v: f64| {
            c.apply(HeadPose {
                yaw_deg: v,
                ..Default::default()
            })
            .yaw_deg
        };
        assert_eq!(at(2.9), 0.0);
        assert_eq!(at(-2.9), 0.0);
        // Just past the edge the output is near zero, not a 3° jump.
        assert!(at(3.1).abs() < 0.2, "{}", at(3.1));
        assert!(at(-20.0) < 0.0, "odd-symmetric");
    }

    #[test]
    fn a_power_curve_is_gentle_at_the_centre_and_reaches_full_output_at_the_edge() {
        let mut c = HeadCurves::default();
        *c.axis_mut("yaw").unwrap() = AxisResponse {
            deadzone_deg: 0.0,
            input_max_deg: 30.0,
            output_max_deg: 90.0,
            curve: Curve::Power(2.0),
            clamp_deg: f64::INFINITY,
        };
        let at = |v: f64| {
            c.apply(HeadPose {
                yaw_deg: v,
                ..Default::default()
            })
            .yaw_deg
        };
        // A linear 3x gain would give 15°; the curve gives 2.5°.
        assert!((at(5.0) - 2.5).abs() < 1e-9, "{}", at(5.0));
        assert!((at(30.0) - 90.0).abs() < 1e-9);
        assert!((at(45.0) - 90.0).abs() < 1e-9, "saturates past input_max");
        // Other axes untouched.
        assert_eq!(c.apply(pose()).roll_deg, pose().roll_deg);
    }

    #[test]
    fn every_axis_name_resolves() {
        let mut c = HeadCurves::default();
        for n in AXIS_NAMES {
            assert!(c.axis_mut(n).is_some(), "{n}");
        }
        assert!(c.axis_mut("w").is_none());
    }
}
