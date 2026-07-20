//! Fullscreen follow-the-dot calibration flow. The user picks Quick/Full, then
//! follows a pulsing dot; each point is sampled by the device thread (see
//! `device::DeviceCommand::Cal*`). The point sets + `CalMode` are unit-tested;
//! the GTK window + cairo dot (added next) are live-validated.

/// Calibration point sets (normalized, top-left origin, center-first — the
/// original's Guest (5) and recalibration (9) sets, verbatim).
pub const QUICK_5: [(f64, f64); 5] = [(0.5, 0.5), (0.1, 0.9), (0.5, 0.1), (0.9, 0.9), (0.5, 0.5)];
pub const FULL_9: [(f64, f64); 9] = [
    (0.5, 0.5),
    (0.1, 0.9),
    (0.5, 0.1),
    (0.9, 0.9),
    (0.1, 0.1),
    (0.5, 0.9),
    (0.9, 0.1),
    (0.1, 0.5),
    (0.9, 0.5),
];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CalMode {
    Quick,
    Full,
}

impl CalMode {
    /// The stimulus points for this mode.
    pub fn points(self) -> &'static [(f64, f64)] {
        match self {
            CalMode::Quick => &QUICK_5,
            CalMode::Full => &FULL_9,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_sets_have_expected_counts_and_start_centered() {
        assert_eq!(CalMode::Quick.points().len(), 5);
        assert_eq!(CalMode::Full.points().len(), 9);
        assert_eq!(CalMode::Quick.points()[0], (0.5, 0.5));
        assert_eq!(CalMode::Full.points()[0], (0.5, 0.5));
    }

    #[test]
    fn all_points_are_within_unit_square() {
        for m in [CalMode::Quick, CalMode::Full] {
            for &(x, y) in m.points() {
                assert!((0.0..=1.0).contains(&x), "x in range: {x}");
                assert!((0.0..=1.0).contains(&y), "y in range: {y}");
            }
        }
    }
}
