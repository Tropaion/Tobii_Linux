//! Pure decision logic: given calibration/display state, what should the GUI do?

use crate::CalMeta;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecommendReason {
    OtherScreen,
    GeometryChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalAction {
    None,
    ForceSetup,
    ForceCalibration,
    RecommendCalibration(RecommendReason),
}

/// Decide the calibration action for the current state.
///
/// `active_monitor` is the id of the screen the tracker is on (None if it can't
/// be identified). `current_fingerprint` is `DisplaySetup::fingerprint()` of the
/// geometry currently configured.
pub fn decide(
    display_configured: bool,
    cal: Option<&CalMeta>,
    active_monitor: Option<&str>,
    current_fingerprint: u64,
) -> CalAction {
    if !display_configured {
        return CalAction::ForceSetup;
    }
    let Some(cal) = cal else {
        return CalAction::ForceCalibration;
    };
    // Screen mismatch only when we can identify both sides.
    if let (Some(active), Some(bound)) = (active_monitor, cal.monitor_id.as_deref()) {
        if active != bound {
            return CalAction::RecommendCalibration(RecommendReason::OtherScreen);
        }
    }
    if cal.display_fingerprint != current_fingerprint {
        return CalAction::RecommendCalibration(RecommendReason::GeometryChanged);
    }
    CalAction::None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(monitor: &str, fp: u64) -> CalMeta {
        CalMeta {
            monitor_id: Some(monitor.into()),
            created_utc: 0,
            mode: "full".into(),
            display_fingerprint: fp,
        }
    }

    #[test]
    fn no_display_setup_forces_setup() {
        assert_eq!(
            decide(false, None, Some("SAM7454"), 1),
            CalAction::ForceSetup
        );
    }
    #[test]
    fn display_but_no_calibration_forces_calibration() {
        assert_eq!(
            decide(true, None, Some("SAM7454"), 1),
            CalAction::ForceCalibration
        );
    }
    #[test]
    fn wrong_screen_recommends_recalibration() {
        let m = meta("DELA042", 1);
        assert_eq!(
            decide(true, Some(&m), Some("SAM7454"), 1),
            CalAction::RecommendCalibration(RecommendReason::OtherScreen)
        );
    }
    #[test]
    fn changed_geometry_recommends_recalibration() {
        let m = meta("SAM7454", 111);
        assert_eq!(
            decide(true, Some(&m), Some("SAM7454"), 222),
            CalAction::RecommendCalibration(RecommendReason::GeometryChanged)
        );
    }
    #[test]
    fn matching_calibration_is_silent() {
        let m = meta("SAM7454", 999);
        assert_eq!(
            decide(true, Some(&m), Some("SAM7454"), 999),
            CalAction::None
        );
    }
    #[test]
    fn unknown_active_monitor_skips_screen_check() {
        // Can't identify the screen → don't spuriously force/recommend on screen id;
        // still catch a geometry change.
        let m = meta("SAM7454", 5);
        assert_eq!(decide(true, Some(&m), None, 5), CalAction::None);
        assert_eq!(
            decide(true, Some(&m), None, 6),
            CalAction::RecommendCalibration(RecommendReason::GeometryChanged)
        );
    }
}
