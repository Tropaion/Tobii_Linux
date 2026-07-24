//! Pure timing and wording logic for the eye-preview + range-check step.
//!
//! This module contains only the business logic — deciding when to show which
//! guidance text, and when to auto-advance or offer a fallback button.
//! GTK/UI wiring for this logic lives in a later task (not in this module).
//!
//! Tick cadence is 33 ms (matching all other flows in the app), so:
//! - `EYE_TEXT_TICKS = 60` ≈ 2 seconds
//! - `CENTERED_DWELL_TICKS = 45` ≈ 1.5 seconds
//! - `STUCK_FALLBACK_TICKS = 450` ≈ 15 seconds

use crate::eyeview::Guidance;

pub const EYE_TEXT_TICKS: u32 = 60;
pub const CENTERED_DWELL_TICKS: u32 = 45;
pub const STUCK_FALLBACK_TICKS: u32 = 450;

/// Guidance text for the eye-preview step based on elapsed ticks and current guidance.
///
/// For the first `EYE_TEXT_TICKS` (~2 seconds), always shows the intro line.
/// After that, switches on the current `Guidance` to show distance/centering advice.
pub fn message(ticks: u32, guidance: Guidance) -> &'static str {
    if ticks < EYE_TEXT_TICKS {
        "These are your eyes..."
    } else {
        match guidance {
            Guidance::MoveCloser => "Move closer.",
            Guidance::MoveBack => "Move back a little.",
            Guidance::NoEyes => "We can't find your eyes. Sit in front of the screen.",
            Guidance::OffCenter | Guidance::Centered => "Find out how much room you have to move.",
        }
    }
}

/// Decide whether to auto-advance to the next step.
///
/// Returns `true` only after a continuous `CENTERED_DWELL_TICKS` (~1.5 seconds)
/// of `Guidance::Centered` readings.
pub fn should_advance(centered_ticks: u32) -> bool {
    centered_ticks >= CENTERED_DWELL_TICKS
}

/// Decide whether to offer a "Continue anyway" fallback button.
///
/// Returns `true` once the total time on this step reaches `STUCK_FALLBACK_TICKS`
/// (~15 seconds), allowing the user to skip if they are not able to center.
pub fn should_offer_fallback(ticks: u32) -> bool {
    ticks >= STUCK_FALLBACK_TICKS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_intro_regardless_of_guidance() {
        // First EYE_TEXT_TICKS should always show the intro line, no matter
        // what guidance is being reported. This lets the user orient.
        let intro = "These are your eyes...";
        for ticks in 0..EYE_TEXT_TICKS {
            assert_eq!(
                message(ticks, Guidance::NoEyes),
                intro,
                "tick {}: should show intro even with NoEyes",
                ticks
            );
            assert_eq!(
                message(ticks, Guidance::MoveCloser),
                intro,
                "tick {}: should show intro even with MoveCloser",
                ticks
            );
            assert_eq!(
                message(ticks, Guidance::Centered),
                intro,
                "tick {}: should show intro even with Centered",
                ticks
            );
        }
    }

    #[test]
    fn message_switches_after_intro() {
        // After intro ticks expire, text should switch based on guidance.
        let t = EYE_TEXT_TICKS;

        assert_eq!(message(t, Guidance::MoveCloser), "Move closer.");
        assert_eq!(message(t, Guidance::MoveBack), "Move back a little.");
        assert_eq!(
            message(t, Guidance::NoEyes),
            "We can't find your eyes. Sit in front of the screen."
        );
        assert_eq!(
            message(t, Guidance::Centered),
            "Find out how much room you have to move."
        );
        assert_eq!(
            message(t, Guidance::OffCenter),
            "Find out how much room you have to move."
        );
    }

    #[test]
    fn should_advance_requires_full_dwell() {
        // Auto-advance should only happen after reaching CENTERED_DWELL_TICKS.
        let threshold = CENTERED_DWELL_TICKS;

        // Just shy of the threshold: should not advance.
        assert!(!should_advance(threshold - 1));

        // At and after threshold: should advance.
        assert!(should_advance(threshold));
        assert!(should_advance(threshold + 1));
        assert!(should_advance(threshold + 100));
    }

    #[test]
    fn should_offer_fallback_after_stuck_timeout() {
        // The fallback button should only appear once we've been stuck for
        // STUCK_FALLBACK_TICKS, allowing an escape route for inaccessible
        // configurations.
        let threshold = STUCK_FALLBACK_TICKS;

        // Well before timeout: no fallback.
        assert!(!should_offer_fallback(threshold - 100));
        assert!(!should_offer_fallback(threshold - 1));

        // At and after timeout: offer fallback.
        assert!(should_offer_fallback(threshold));
        assert!(should_offer_fallback(threshold + 1));
        assert!(should_offer_fallback(threshold + 1000));
    }
}
