//! Pure math for the visual tracker-alignment step of display setup.
//!
//! The two on-screen vertical lines are dragged to the physical left/right ends
//! of the eye tracker (whose real width is known). From their normalized x
//! positions we derive the screen's physical width and the tracker's horizontal
//! offset — the same shape of algorithm the original Tobii software uses.

/// Span in mm that the two alignment lines are meant to bracket: the distance
/// between the reference marks on the eye tracker.
///
/// **MEASURED on a physical ET5 (2026-07-21): 184 mm.**
///
/// This replaced a long-standing 376.3, which was wrong by a factor of 2.04 and
/// documented as "verified" when it never had been. Two independent lines of
/// evidence agree on 184:
/// - Direct measurement of the marks on the device.
/// - Back-calculation: a user dragging the lines accurately onto those marks
///   produced a gap of 0.15475 of screen width on a monitor EDID reports as
///   1193 mm, implying 184.6 mm — 0.3% from the measured figure. With 376.3
///   that same drag derived a 2431.7 mm "screen", which threw gaze mapping off
///   by centimetres.
///
/// Note this is the span between the *marks*, not the device's overall length
/// (a published ET5 is 285 mm long) — which is why the old value being longer
/// than the whole tracker should have been caught immediately.
pub const EYE_TRACKER_WIDTH_MM: f64 = 184.0;
/// Line clamps (normalized screen x) + minimum gap between the two lines.
pub const MIN_LINE: f64 = 0.02;
pub const MAX_LINE: f64 = 0.98;
pub const MIN_GAP: f64 = 0.05;

/// Physical screen size + horizontal tracker offset derived from the lines.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Alignment {
    pub width_mm: f64,
    pub height_mm: f64,
    pub offset_x_mm: f64,
}

/// Clamp a pair of line positions into `[MIN_LINE, MAX_LINE]`, order them
/// left≤right, and enforce `MIN_GAP` between them.
pub fn clamp_lines(l: f64, r: f64) -> (f64, f64) {
    let l = l.clamp(MIN_LINE, MAX_LINE);
    let r = r.clamp(MIN_LINE, MAX_LINE);
    let (mut lo, mut hi) = if l <= r { (l, r) } else { (r, l) };
    if hi - lo < MIN_GAP {
        let mid = (lo + hi) / 2.0;
        hi = (mid + MIN_GAP / 2.0).min(MAX_LINE);
        lo = hi - MIN_GAP;
    }
    (lo, hi)
}

/// From normalized line positions and the screen aspect ratio (width/height),
/// derive the physical width/height (mm) and the horizontal offset of the
/// screen centre from the tracker (mm).
pub fn alignment_from_lines(l: f64, r: f64, aspect_ratio: f64) -> Alignment {
    let (l, r) = clamp_lines(l, r);
    let width_mm = EYE_TRACKER_WIDTH_MM / (r - l);
    let height_mm = if aspect_ratio > 0.0 {
        width_mm / aspect_ratio
    } else {
        width_mm
    };
    let offset_x_mm = ((l + r) / 2.0 - 0.5) * width_mm;
    Alignment {
        width_mm,
        height_mm,
        offset_x_mm,
    }
}

/// Inverse of [`alignment_from_lines`] (ignoring height): seed line positions
/// from a known physical width + horizontal offset, to pre-fill the alignment
/// from a saved config or an EDID size hint.
pub fn lines_from_width_offset(width_mm: f64, offset_x_mm: f64) -> (f64, f64) {
    if width_mm <= 0.0 {
        return (0.25, 0.75);
    }
    let half = EYE_TRACKER_WIDTH_MM / width_mm / 2.0;
    let center = 0.5 + offset_x_mm / width_mm;
    clamp_lines(center - half, center + half)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_world_drag_derives_the_measured_screen_width() {
        // Ground truth from a physical setup (2026-07-21): the ET5's reference
        // marks measure 184 mm apart, and a user dragging the lines onto them
        // on a Samsung Odyssey G93SC produced a gap of 0.15475 of screen width.
        // EDID reports that panel as 1193 mm wide, so the derivation must land
        // close to it. This is the case that exposed the old 376.3 constant,
        // which turned the very same drag into a 2431.7 mm "screen".
        let gap = 0.154_747_f64;
        let (l, r) = (0.5 - gap / 2.0, 0.5 + gap / 2.0);
        let a = alignment_from_lines(l, r, 3.55);
        assert!(
            (a.width_mm - 1193.0).abs() < 30.0,
            "derived {:.0} mm from a correct drag, expected ~1193 mm (EDID)",
            a.width_mm
        );
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn centered_lines_give_centered_offset() {
        // Lines at 0.25/0.75 span half the screen -> width = tracker/0.5, offset 0.
        let a = alignment_from_lines(0.25, 0.75, 16.0 / 9.0);
        assert!(approx(a.width_mm, EYE_TRACKER_WIDTH_MM / 0.5));
        assert!(approx(a.offset_x_mm, 0.0));
        assert!(approx(a.height_mm, a.width_mm / (16.0 / 9.0)));
    }

    #[test]
    fn off_center_lines_give_nonzero_offset() {
        // Shifted right -> positive offset_x.
        let a = alignment_from_lines(0.4, 0.9, 1.0);
        assert!(a.offset_x_mm > 0.0);
    }

    #[test]
    fn width_offset_round_trips_through_lines() {
        let (w, ox) = (752.6, 40.0);
        let (l, r) = lines_from_width_offset(w, ox);
        let a = alignment_from_lines(l, r, 1.0);
        assert!(approx(a.width_mm, w));
        assert!(approx(a.offset_x_mm, ox));
    }

    #[test]
    fn clamp_enforces_bounds_and_gap() {
        let (l, r) = clamp_lines(-1.0, 5.0);
        assert!(l >= MIN_LINE && r <= MAX_LINE);
        let (l, r) = clamp_lines(0.5, 0.51); // too close
        assert!(r - l >= MIN_GAP - 1e-9);
    }

    #[test]
    fn clamp_orders_reversed_inputs() {
        let (l, r) = clamp_lines(0.8, 0.2);
        assert!(l < r);
    }
}
