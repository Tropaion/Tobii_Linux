//! Pure timing and wording logic for the eye-preview + range-check step.
//!
//! This module contains only the business logic — deciding when to show which
//! guidance text, and when to auto-advance or offer a fallback button.
//! GTK/UI wiring for this logic lives in a later task (not in this module).
//!
//! Tick cadence is 33 ms (matching all other flows in the app), so:
//! - `EYE_TEXT_TICKS = 182` ≈ 6 seconds (matches the decompiled original's
//!   `EyesPositioningViewModel.InitialTime`)
//! - `CENTERED_DWELL_TICKS = 91` ≈ 3 seconds continuous (matches the
//!   original's `PresenceTime`)
//! - `STUCK_FALLBACK_TICKS = 450` ≈ 15 seconds

use crate::eyeview::Guidance;

pub const EYE_TEXT_TICKS: u32 = 182; // ~6000ms (original's InitialTime), was 60 (~2s)
pub const CENTERED_DWELL_TICKS: u32 = 91; // ~3000ms continuous (original's PresenceTime), was 45 (~1.5s)
pub const STUCK_FALLBACK_TICKS: u32 = 450;

/// How long a gap in `Guidance::Centered` readings (a blink, a momentary
/// off-center glance) is tolerated without resetting the centered-dwell
/// streak — matches the decompiled original's `AbsenceOffset` (~500ms).
pub const CENTERED_GAP_TOLERANCE_TICKS: u32 = 15;

/// Guidance text for the eye-preview step based on elapsed ticks and current guidance.
///
/// For the first `EYE_TEXT_TICKS` (~6 seconds), always shows the intro line.
/// After that, switches on the current `Guidance` to show distance/centering advice.
///
/// Every string here is the original Windows software's own, recovered verbatim
/// from its `LanguageResources` string table: the intro pair is
/// `EyesPositioning_FullScreenMessage_One`/`_Two`, the nudges are
/// `_MoveCloser`/`_LeanBack`/`_MoveRight`/`_MoveLeft`/`_MoveDown`/`_MoveUp`,
/// and the eyes-lost line is `_CanNotTrackBothEyes`.
pub fn message(ticks: u32, guidance: Guidance) -> &'static str {
    if ticks < EYE_TEXT_TICKS {
        "These are your eyes..."
    } else {
        match guidance {
            Guidance::MoveCloser => "Move closer",
            Guidance::MoveBack => "Lean back",
            Guidance::MoveRight => "Move right",
            Guidance::MoveLeft => "Move left",
            Guidance::MoveDown => "Move down",
            Guidance::MoveUp => "Move up",
            Guidance::NoEyes => "Are you there?",
            Guidance::Centered => "See how much you can move around.",
        }
    }
}

/// Decide whether to auto-advance to the next step.
///
/// Requires BOTH: the ~6s intro window (`EYE_TEXT_TICKS`) to have elapsed
/// (so every user sees at least a moment of the range-check guidance text
/// from `message`, not just the intro line) AND a continuous
/// `CENTERED_DWELL_TICKS` (~3s, tolerating brief gaps — see
/// `update_centered_streak`) of `Guidance::Centered` readings.
pub fn should_advance(ticks: u32, centered_ticks: u32) -> bool {
    ticks >= EYE_TEXT_TICKS && centered_ticks >= CENTERED_DWELL_TICKS
}

/// Updates the continuous "centered" dwell streak by one tick, tolerating
/// gaps up to `CENTERED_GAP_TOLERANCE_TICKS` in a row — a brief blink or
/// momentary off-center reading does not reset progress toward
/// `CENTERED_DWELL_TICKS`, only a SUSTAINED departure does. Call once per
/// tick with the previous tick's own returned `(centered_ticks, gap_ticks)`
/// pair (start both at 0).
pub fn update_centered_streak(
    guidance: Guidance,
    centered_ticks: u32,
    gap_ticks: u32,
) -> (u32, u32) {
    if guidance == Guidance::Centered {
        (centered_ticks + 1, 0)
    } else if gap_ticks + 1 < CENTERED_GAP_TOLERANCE_TICKS {
        (centered_ticks, gap_ticks + 1) // brief gap: hold the streak
    } else {
        (0, gap_ticks + 1) // gap exceeded tolerance: streak lost
    }
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
        // After intro ticks expire, text should switch based on guidance — and
        // every string should be the original software's own wording.
        let t = EYE_TEXT_TICKS;

        assert_eq!(message(t, Guidance::MoveCloser), "Move closer");
        assert_eq!(message(t, Guidance::MoveBack), "Lean back");
        assert_eq!(message(t, Guidance::NoEyes), "Are you there?");
        assert_eq!(
            message(t, Guidance::Centered),
            "See how much you can move around."
        );
    }

    #[test]
    fn message_names_every_nudge_direction() {
        // The original tells the user WHICH WAY to move rather than a generic
        // "center yourself", so each direction must have its own wording.
        let t = EYE_TEXT_TICKS;
        assert_eq!(message(t, Guidance::MoveRight), "Move right");
        assert_eq!(message(t, Guidance::MoveLeft), "Move left");
        assert_eq!(message(t, Guidance::MoveDown), "Move down");
        assert_eq!(message(t, Guidance::MoveUp), "Move up");
    }

    #[test]
    fn should_advance_requires_full_dwell() {
        // Auto-advance should only happen after reaching CENTERED_DWELL_TICKS,
        // given the intro window has already elapsed.
        let threshold = CENTERED_DWELL_TICKS;
        let ticks = EYE_TEXT_TICKS + 100; // well past the intro window

        // Just shy of the threshold: should not advance.
        assert!(!should_advance(ticks, threshold - 1));

        // At and after threshold: should advance.
        assert!(should_advance(ticks, threshold));
        assert!(should_advance(ticks, threshold + 1));
        assert!(should_advance(ticks, threshold + 100));
    }

    #[test]
    fn should_advance_requires_intro_window_to_elapse() {
        // Even with the centered dwell fully satisfied, auto-advance must not
        // fire until the intro-text window (EYE_TEXT_TICKS) has also elapsed —
        // otherwise a well-positioned user never sees the range-check guidance
        // text from `message` before calibration starts.
        let centered_ticks = CENTERED_DWELL_TICKS + 100; // well past dwell

        assert!(!should_advance(0, centered_ticks));
        assert!(!should_advance(EYE_TEXT_TICKS - 1, centered_ticks));

        // Once the intro window elapses too, it should advance.
        assert!(should_advance(EYE_TEXT_TICKS, centered_ticks));
    }

    #[test]
    fn update_centered_streak_counts_up_while_centered() {
        let mut centered_ticks = 0;
        let mut gap_ticks = 0;
        for expected in 1..=5 {
            (centered_ticks, gap_ticks) =
                update_centered_streak(Guidance::Centered, centered_ticks, gap_ticks);
            assert_eq!(centered_ticks, expected);
            assert_eq!(gap_ticks, 0);
        }
    }

    #[test]
    fn update_centered_streak_tolerates_a_brief_gap() {
        // Build up a streak, then take a single-tick gap (e.g. a blink) and
        // come back to Centered — the streak must NOT have been reset.
        let mut centered_ticks = 0;
        let mut gap_ticks = 0;
        for _ in 0..10 {
            (centered_ticks, gap_ticks) =
                update_centered_streak(Guidance::Centered, centered_ticks, gap_ticks);
        }
        assert_eq!(centered_ticks, 10);

        // One tick of NoEyes (the blink): streak is held, not reset.
        (centered_ticks, gap_ticks) =
            update_centered_streak(Guidance::NoEyes, centered_ticks, gap_ticks);
        assert_eq!(
            centered_ticks, 10,
            "a single-tick gap must not reset the streak"
        );
        assert_eq!(gap_ticks, 1);

        // Back to Centered: streak resumes counting up from where it was, and
        // the gap counter clears.
        (centered_ticks, gap_ticks) =
            update_centered_streak(Guidance::Centered, centered_ticks, gap_ticks);
        assert_eq!(centered_ticks, 11);
        assert_eq!(gap_ticks, 0);
    }

    #[test]
    fn update_centered_streak_resets_after_sustained_gap() {
        // Build up a streak, then hold a non-Centered reading for
        // CENTERED_GAP_TOLERANCE_TICKS consecutive ticks in a row — long
        // enough that tolerance is exhausted and the streak must reset.
        let mut centered_ticks = 0;
        let mut gap_ticks = 0;
        for _ in 0..10 {
            (centered_ticks, gap_ticks) =
                update_centered_streak(Guidance::Centered, centered_ticks, gap_ticks);
        }
        assert_eq!(centered_ticks, 10);

        let mut reset_at = None;
        for tick in 1..=CENTERED_GAP_TOLERANCE_TICKS {
            (centered_ticks, gap_ticks) =
                update_centered_streak(Guidance::MoveLeft, centered_ticks, gap_ticks);
            if centered_ticks == 0 && reset_at.is_none() {
                reset_at = Some(tick);
            }
        }
        assert_eq!(
            reset_at,
            Some(CENTERED_GAP_TOLERANCE_TICKS),
            "streak should reset exactly once tolerance is exhausted"
        );
        assert_eq!(centered_ticks, 0);
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
