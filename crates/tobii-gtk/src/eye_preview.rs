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

/// Consecutive-tick threshold for leaving a shown `Guidance::Centered`
/// message for anything OTHER than `Guidance::NoEyes` (matches the
/// original's `MaxCountOfInvalidGazeDataFrom2To3`). A direct
/// `Centered -> NoEyes` transition is gated by BOTH this AND
/// `RETURN_TO_NO_EYES_DEBOUNCE_TICKS` (see `debounced_guidance`'s two
/// sequential — not `else if` — checks), so it actually takes the longer
/// threshold, not this one.
pub const LEAVE_CENTERED_DEBOUNCE_TICKS: u32 = 11;
/// Consecutive-tick threshold for falling all the way back to the `NoEyes`
/// message from any other shown state (matches the original's
/// `MaxCountOfInvalidGazeDataFrom2To1`/`...From3To1`).
pub const RETURN_TO_NO_EYES_DEBOUNCE_TICKS: u32 = 49;

/// Guidance text for the eye-preview step based on elapsed ticks and current guidance.
///
/// For the first `EYE_TEXT_TICKS` (~6 seconds), always shows the intro line.
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

/// Debounces the raw per-frame `Guidance` into a stable value for `message()`
/// to render, so brief noisy flickers don't visibly change the message every
/// frame. `raw` is this tick's live reading; `shown` is whatever the LAST
/// call to this function returned (start at `Guidance::NoEyes` before any
/// reading exists); `unstable_ticks` is how many consecutive ticks `raw` has
/// differed from `shown` (the caller tracks this: reset to 0 whenever
/// `raw == shown`, else increment — see the call site in `calibrate_flow.rs`).
/// Returns the guidance to actually display this tick.
///
/// Note the two threshold checks below are sequential `if`s, not
/// `else if`-linked: a `Centered -> NoEyes` transition matches BOTH, so it is
/// gated by the longer `RETURN_TO_NO_EYES_DEBOUNCE_TICKS` threshold even
/// though it is also a "leaving Centered" case.
pub fn debounced_guidance(raw: Guidance, shown: Guidance, unstable_ticks: u32) -> Guidance {
    if raw == shown {
        return shown;
    }
    if shown == Guidance::Centered && unstable_ticks < LEAVE_CENTERED_DEBOUNCE_TICKS {
        return shown; // not yet enough consecutive frames to leave Centered
    }
    if raw == Guidance::NoEyes
        && shown != Guidance::NoEyes
        && unstable_ticks < RETURN_TO_NO_EYES_DEBOUNCE_TICKS
    {
        return shown; // not yet enough consecutive frames to fall back to NoEyes
    }
    raw
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
                update_centered_streak(Guidance::OffCenter, centered_ticks, gap_ticks);
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
    fn debounced_guidance_holds_when_raw_matches_shown() {
        // raw == shown always returns shown, regardless of unstable_ticks.
        assert_eq!(
            debounced_guidance(Guidance::Centered, Guidance::Centered, 0),
            Guidance::Centered
        );
        assert_eq!(
            debounced_guidance(Guidance::NoEyes, Guidance::NoEyes, 999),
            Guidance::NoEyes
        );
    }

    #[test]
    fn debounced_guidance_leaving_centered_requires_threshold() {
        // Below the threshold: held at Centered.
        assert_eq!(
            debounced_guidance(
                Guidance::OffCenter,
                Guidance::Centered,
                LEAVE_CENTERED_DEBOUNCE_TICKS - 1
            ),
            Guidance::Centered
        );
        // At and above the threshold: switches to the raw reading.
        assert_eq!(
            debounced_guidance(
                Guidance::OffCenter,
                Guidance::Centered,
                LEAVE_CENTERED_DEBOUNCE_TICKS
            ),
            Guidance::OffCenter
        );
        assert_eq!(
            debounced_guidance(
                Guidance::OffCenter,
                Guidance::Centered,
                LEAVE_CENTERED_DEBOUNCE_TICKS + 5
            ),
            Guidance::OffCenter
        );
    }

    #[test]
    fn debounced_guidance_falling_to_no_eyes_requires_threshold() {
        // Below the threshold: held at the previously-shown message.
        assert_eq!(
            debounced_guidance(
                Guidance::NoEyes,
                Guidance::MoveCloser,
                RETURN_TO_NO_EYES_DEBOUNCE_TICKS - 1
            ),
            Guidance::MoveCloser
        );
        // At and above the threshold: falls back to NoEyes.
        assert_eq!(
            debounced_guidance(
                Guidance::NoEyes,
                Guidance::MoveCloser,
                RETURN_TO_NO_EYES_DEBOUNCE_TICKS
            ),
            Guidance::NoEyes
        );
        assert_eq!(
            debounced_guidance(
                Guidance::NoEyes,
                Guidance::MoveCloser,
                RETURN_TO_NO_EYES_DEBOUNCE_TICKS + 5
            ),
            Guidance::NoEyes
        );
    }

    #[test]
    fn debounced_guidance_centered_to_no_eyes_requires_the_longer_threshold() {
        // A direct Centered -> NoEyes transition matches BOTH sequential
        // checks in debounced_guidance: "leaving Centered" (shown ==
        // Centered) AND "falling back to NoEyes" (raw == NoEyes && shown !=
        // NoEyes). Because the checks are separate `if`s, not `else if`s,
        // clearing the shorter LEAVE_CENTERED_DEBOUNCE_TICKS gate (11) is not
        // enough — the second check still blocks the switch until the
        // longer RETURN_TO_NO_EYES_DEBOUNCE_TICKS threshold (49) is also
        // met. This test guards against a future refactor accidentally
        // turning these into `else if`s, which would let this transition
        // through early.
        assert_eq!(
            debounced_guidance(
                Guidance::NoEyes,
                Guidance::Centered,
                RETURN_TO_NO_EYES_DEBOUNCE_TICKS - 1
            ),
            Guidance::Centered,
            "still short of the longer threshold: must stay held at Centered"
        );
        assert_eq!(
            debounced_guidance(
                Guidance::NoEyes,
                Guidance::Centered,
                RETURN_TO_NO_EYES_DEBOUNCE_TICKS
            ),
            Guidance::NoEyes,
            "at the longer threshold: switches to NoEyes"
        );
    }

    #[test]
    fn debounced_guidance_other_transitions_are_immediate() {
        // A transition that is neither "leaving Centered" nor "falling back
        // to NoEyes" switches immediately, even at unstable_ticks == 0.
        assert_eq!(
            debounced_guidance(Guidance::OffCenter, Guidance::MoveCloser, 0),
            Guidance::OffCenter
        );
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
