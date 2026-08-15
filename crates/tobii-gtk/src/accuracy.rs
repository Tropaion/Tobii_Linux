//! Measure where the device *thinks* you are looking versus where you actually
//! are, across the whole screen — and say which of the competing explanations
//! the numbers support.
//!
//! # Why this exists
//!
//! Edge gaze error on a wide screen has several candidate causes, and they
//! **all produce the same qualitative symptom**: accurate at the centre,
//! worsening toward the sides. Guessing between them has a perfect record of
//! failure on this project:
//!
//! - A runtime **curvature** correction was reasoned out from geometry,
//!   shipped, and reverted in `8b21938` — accurate at the centre, centimetres
//!   out at the sides, the signature of double-counting something the
//!   calibration had already absorbed.
//! - **Calibration coverage** looked compelling too: `capped_area` confines the
//!   stimulus points to 600 mm, so on a 1193 mm panel every point lands between
//!   x = 0.30 and x = 0.70. But `calibration_area` is a port of the original's
//!   own `CalibrationAreaCalculator`, so the original confines them identically.
//!   Something both sides do cannot explain a difference between them.
//! - **Plane depth** (`offset_z_mm`) has never been measured on any setup here,
//!   and a depth error produces this shape too.
//!
//! What distinguishes them is not the shape of the symptom but the shape of the
//! *profile*: a depth or width error grows **linearly** with distance from the
//! centre; a curvature mismatch grows **faster than linearly**; a coverage gap
//! is **flat inside the calibrated band and then climbs**. The ratio
//! `error / distance-from-centre` separates them and nothing available by
//! inspection does.
//!
//! So: measure first, and let the numbers pick. This module produces them;
//! [`diagnose`] reads them. It is also the only way to tell whether a later
//! change actually helped — the check that has been missing every round.

/// One stimulus position, in full-screen normalized coordinates.
///
/// Deliberately spanning the literal screen edges (0.02 / 0.98), because the
/// edges are the thing under investigation. Points are ordered centre-outwards
/// so that an aborted run still yields a usable middle.
pub fn targets() -> Vec<(f64, f64)> {
    // Thirteen columns, not nine. The first sweep put only three inside the
    // calibrated band (0.35, 0.50, 0.65) and so could say nothing about the
    // structure *within* it — while the user's report was precisely that
    // accuracy falls off well before the band's edge. Extra columns at 0.26,
    // 0.42, 0.58 and 0.74 make that region resolvable instead of a guess.
    let xs = [
        0.5, 0.42, 0.58, 0.34, 0.66, 0.26, 0.74, 0.18, 0.82, 0.1, 0.9, 0.02, 0.98,
    ];
    let ys = [0.5, 0.15, 0.85];
    let mut out = Vec::with_capacity(xs.len() * ys.len());
    for (i, &x) in xs.iter().enumerate() {
        // Alternate the row order per column so consecutive targets are never
        // in the same place twice running — a target that reappears where the
        // last one was invites the eye to stay put instead of re-fixating.
        for &y in ys.iter().cycle().skip(i % ys.len()).take(ys.len()) {
            out.push((x, y));
        }
    }
    out
}

/// Ticks (at the 33 ms flow cadence) to let the eye land before believing it.
pub const SETTLE_TICKS: u32 = 30;
/// Ticks of samples averaged per target once settled.
pub const SAMPLE_TICKS: u32 = 30;

/// One measured target: where the dot was, and where the device said you looked.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measurement {
    pub target: (f64, f64),
    pub reported: (f64, f64),
    /// Samples that went into `reported`. Zero means the device never produced
    /// a valid gaze point here — which is itself the answer for that target.
    pub samples: usize,
    /// Where the eyes actually were, tracker-space mm, midway between them.
    ///
    /// Recorded per target rather than assumed, because the quantity that
    /// matters most here — how far the eye had to rotate to reach the dot —
    /// depends entirely on it, and a profile read against a *guessed* head
    /// position produces a confident and completely wrong story. This field
    /// exists because that is exactly what happened: an eye position taken from
    /// an unrelated capture made a symmetric, angle-driven falloff look like an
    /// asymmetric seating problem.
    pub eye_mm: Option<[f64; 3]>,
    /// The device's **per-eye** gaze points (columns `0x05` / `0x0b`), which it
    /// reports alongside the fused one.
    ///
    /// Recorded because the fused point cannot distinguish "both eyes agree and
    /// are wrong" from "one eye is dragging the average". Those need opposite
    /// responses, and the measured error here is 2.3x worse looking left than
    /// right — an asymmetry no screen-geometry cause can produce, since
    /// curvature, plane width and plane depth all act symmetrically about the
    /// screen centre. Two eyes are the only asymmetric thing in the system.
    pub reported_l: Option<(f64, f64)>,
    pub reported_r: Option<(f64, f64)>,
}

impl Measurement {
    /// Signed horizontal error in screen widths. Positive = reported to the
    /// right of the dot.
    pub fn error_x(&self) -> f64 {
        self.reported.0 - self.target.0
    }
    /// How far this target sits from the horizontal centre, in screen widths.
    pub fn offset_x(&self) -> f64 {
        self.target.0 - 0.5
    }

    /// Signed horizontal error of one eye's own gaze point.
    pub fn eye_error_x(&self, right: bool) -> Option<f64> {
        let r = if right {
            self.reported_r
        } else {
            self.reported_l
        }?;
        Some(r.0 - self.target.0)
    }

    /// How far the eye had to rotate horizontally to reach this target, in
    /// degrees, from the eye position actually recorded for it.
    ///
    /// Horizontal only: the display's tilt is about the horizontal axis, so it
    /// does not enter this. `offset_x_mm` is where the display centre sits
    /// relative to the tracker, so subtracting it puts the eye in screen-centre
    /// coordinates. Approximate in that it uses the eye's tracker-space depth
    /// rather than a true perpendicular to the tilted plane; over the few
    /// degrees of tilt involved that is worth far less than the honesty of
    /// using a measured head position at all.
    pub fn gaze_angle_deg(&self, width_mm: f64, offset_x_mm: f64) -> Option<f64> {
        let eye = self.eye_mm?;
        if eye[2].abs() < 1.0 {
            return None;
        }
        let target_mm = self.offset_x() * width_mm;
        let eye_from_centre = eye[0] - offset_x_mm;
        Some((target_mm - eye_from_centre).atan2(eye[2]).to_degrees())
    }
}

/// Median of each axis independently. Median rather than mean because a blink
/// or a glance away produces a wild outlier, and one of those would drag a mean
/// far enough to invent an error that is not there.
pub fn aggregate(samples: &[(f64, f64)]) -> Option<(f64, f64)> {
    if samples.is_empty() {
        return None;
    }
    let med = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v[v.len() / 2]
    };
    Some((
        med(samples.iter().map(|s| s.0).collect()),
        med(samples.iter().map(|s| s.1).collect()),
    ))
}

/// Mean head position over one target's sampling window. Mean rather than
/// median: a head drifts slowly and smoothly, so there are no outliers to
/// reject, and averaging is the better estimator of where it actually sat.
pub fn mean_eye(samples: &[[f64; 3]]) -> Option<[f64; 3]> {
    if samples.is_empty() {
        return None;
    }
    let n = samples.len() as f64;
    let mut acc = [0.0f64; 3];
    for s in samples {
        for k in 0..3 {
            acc[k] += s[k];
        }
    }
    Some([acc[0] / n, acc[1] / n, acc[2] / n])
}

/// What the error profile points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// Error per unit offset is flat: a scale error, i.e. the plane is at the
    /// wrong depth or the wrong width. Both are single numbers in the config.
    PlaneGeometry,
    /// Error per unit offset climbs toward the edges: the flat plane cannot
    /// represent the real screen shape.
    Curvature,
    /// Error is small inside the calibrated band and jumps outside it: the
    /// device is extrapolating past its evidence.
    CalibrationCoverage,
    /// The device stops producing gaze data toward the edges. No host-side
    /// arithmetic fixes an eye rotation the sensor cannot resolve; the lever is
    /// where the user sits, or a smaller screen.
    DeviceAngularLimit,
    /// Nothing stands out above the measurement noise.
    NoneApparent,
    /// The profile does not commit. Said out loud rather than resolved by
    /// picking the closest-looking cause, because on this problem every
    /// confident guess so far has been wrong.
    Inconclusive,
}

/// A diagnosis, with the numbers it rests on so it can be argued with.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnosis {
    pub cause: Cause,
    /// `error / offset` for targets inside the calibrated band.
    pub inner_gain: f64,
    /// The same ratio for targets outside it.
    pub outer_gain: f64,
    /// Worst absolute horizontal error seen, in screen widths.
    pub worst_error: f64,
    /// Targets the device never produced a gaze point for.
    pub dead_targets: usize,
    pub detail: String,
}

/// Below this, an error is indistinguishable from fixation noise: people do not
/// hold a fixation to better than roughly half a degree, which at arm's length
/// on a 1193 mm screen is about 0.6% of the width.
const NOISE_FLOOR: f64 = 0.01;

/// How much larger the outer gain must be before the difference is called real
/// rather than scatter. Two independent gains each carrying ~30% noise need a
/// gap well clear of that to mean anything.
const GAIN_RATIO: f64 = 1.8;

/// Read an error profile and name the most likely cause.
///
/// `calibrated_band` is the `(min_x, max_x)` the calibration points actually
/// covered — from `calibration_area::capped_area` composed with the point set,
/// NOT the full screen. Targets outside it are the extrapolated ones.
pub fn diagnose(measurements: &[Measurement], calibrated_band: (f64, f64)) -> Diagnosis {
    let dead_targets = measurements.iter().filter(|m| m.samples == 0).count();
    let usable: Vec<&Measurement> = measurements.iter().filter(|m| m.samples > 0).collect();

    let worst_error = usable
        .iter()
        .map(|m| m.error_x().abs())
        .fold(0.0f64, f64::max);

    // Only targets meaningfully off-centre carry information about a gain:
    // near x = 0.5 the offset is ~0 and error/offset explodes on noise alone.
    let gain = |ms: &[&Measurement]| -> f64 {
        let pts: Vec<f64> = ms
            .iter()
            .filter(|m| m.offset_x().abs() > 0.1)
            .map(|m| m.error_x() / m.offset_x())
            .collect();
        if pts.is_empty() {
            return 0.0;
        }
        pts.iter().sum::<f64>() / pts.len() as f64
    };

    let (lo, hi) = calibrated_band;
    let inside: Vec<&Measurement> = usable
        .iter()
        .copied()
        .filter(|m| m.target.0 >= lo && m.target.0 <= hi)
        .collect();
    let outside: Vec<&Measurement> = usable
        .iter()
        .copied()
        .filter(|m| m.target.0 < lo || m.target.0 > hi)
        .collect();

    let inner_gain = gain(&inside);
    let outer_gain = gain(&outside);

    // Within the calibrated band the device has been shown the answer, so a
    // gain here is the plane's own geometry showing through rather than any
    // failure to extrapolate.
    let inner_is_real = inner_gain.abs() > NOISE_FLOOR;
    let outer_is_worse = outer_gain.abs() > inner_gain.abs().max(NOISE_FLOOR) * GAIN_RATIO;

    // Capture rate outranks every geometric reading. If the device stops
    // answering toward the edges it has run out of eye rotation it can resolve,
    // and no amount of host-side arithmetic recovers a sample that was never
    // produced. Diagnosing "curvature" off the wild errors that surround such a
    // collapse is how a real physical limit gets mistaken for a software bug.
    let capture = |ms: &[&Measurement]| -> f64 {
        let total: usize = ms.len();
        if total == 0 {
            return 1.0;
        }
        ms.iter()
            .filter(|m| m.samples >= SAMPLE_TICKS as usize)
            .count() as f64
            / total as f64
    };
    let edge: Vec<&Measurement> = measurements
        .iter()
        .filter(|m| m.offset_x().abs() > 0.35)
        .collect();
    let middle: Vec<&Measurement> = measurements
        .iter()
        .filter(|m| m.offset_x().abs() <= 0.35)
        .collect();
    let (edge_capture, mid_capture) = (capture(&edge), capture(&middle));

    let (cause, detail) = if !edge.is_empty() && edge_capture < 0.75 && mid_capture >= 0.9 {
        (
            Cause::DeviceAngularLimit,
            format!(
                "the device returns full gaze data for {:.0}% of mid-screen targets but only \
                 {:.0}% at the edges. It is running out of resolvable eye rotation, which is \
                 a property of the sensor and the geometry, not of this code. The levers are \
                 where you sit (further back, and centred) and screen size — not a correction.",
                mid_capture * 100.0,
                edge_capture * 100.0
            ),
        )
    } else if worst_error <= NOISE_FLOOR && dead_targets == 0 {
        (
            Cause::NoneApparent,
            format!(
                "worst error {:.1}% of screen width — at the noise floor",
                worst_error * 100.0
            ),
        )
    } else if inside.len() < 3 || outside.len() < 3 {
        (
            Cause::Inconclusive,
            format!(
                "only {} usable target(s) inside the calibrated band and {} outside — too few \
                 to separate a scale error from a shape error. Re-run with more of the sweep \
                 completed.",
                inside.len(),
                outside.len()
            ),
        )
    } else if outer_is_worse && !inner_is_real {
        (
            Cause::CalibrationCoverage,
            format!(
                "accurate inside the calibrated band ({:.0}%–{:.0}% of the width) and \
                 {:.1}x worse outside it. The device is extrapolating past its evidence: \
                 widen the calibration area.",
                lo * 100.0,
                hi * 100.0,
                (outer_gain.abs() / inner_gain.abs().max(NOISE_FLOOR)),
            ),
        )
    } else if outer_is_worse && inner_is_real {
        (
            Cause::Curvature,
            format!(
                "error per unit offset climbs from {:.3} inside the calibrated band to \
                 {:.3} outside it. Growing faster than linearly is the shape mismatch \
                 between the flat plane and a curved panel, not a scale error.",
                inner_gain, outer_gain
            ),
        )
    } else if inner_is_real {
        (
            Cause::PlaneGeometry,
            format!(
                "error is proportional to distance from centre ({:.3} per unit offset, \
                 near enough the same inside and outside the calibrated band). That is a \
                 scale error: plane width or offset_z_mm, both single config numbers.",
                inner_gain
            ),
        )
    } else {
        (
            Cause::NoneApparent,
            format!(
                "no consistent horizontal trend; worst error {:.1}% of screen width, \
                 {dead_targets} target(s) with no gaze data",
                worst_error * 100.0
            ),
        )
    };

    Diagnosis {
        cause,
        inner_gain,
        outer_gain,
        worst_error,
        dead_targets,
        detail,
    }
}

/// The horizontal band the calibration points actually covered, given the
/// screen size and the point set's own x values.
pub fn calibrated_band(width_mm: f64, height_mm: f64, points: &[(f64, f64)]) -> (f64, f64) {
    let area = crate::calibration_area::capped_area(width_mm, height_mm);
    let xs: Vec<f64> = points
        .iter()
        .map(|&p| crate::calibration_area::remap_point(p, area).0)
        .collect();
    let lo = xs.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if lo.is_finite() && hi.is_finite() {
        (lo, hi)
    } else {
        (0.0, 1.0)
    }
}

/// Render the profile as a table a human can read and argue with.
pub fn report(
    measurements: &[Measurement],
    band: (f64, f64),
    width_mm: f64,
    offset_x_mm: f64,
) -> String {
    let mut s = String::new();
    // The target's y is shown because the three rows per column are different
    // heights, and collapsing them hid a real vertical spread the first time
    // this printed. The gaze angle is shown because it, not the x coordinate,
    // is what the falloff actually tracks.
    s.push_str("  target      gaze     reported   error    error mm   samples\n");
    s.push_str("   x    y     angle\n");
    let mut rows: Vec<&Measurement> = measurements.iter().collect();
    rows.sort_by(|a, b| {
        a.target
            .0
            .partial_cmp(&b.target.0)
            .unwrap()
            .then(a.target.1.partial_cmp(&b.target.1).unwrap())
    });
    for m in rows {
        let angle = match m.gaze_angle_deg(width_mm, offset_x_mm) {
            Some(a) => format!("{a:+6.1}"),
            None => "     ?".to_string(),
        };
        if m.samples == 0 {
            s.push_str(&format!(
                "  {:4.2} {:4.2}  {}      —        —          —         0   <-- no gaze data\n",
                m.target.0, m.target.1, angle
            ));
            continue;
        }
        let inside = m.target.0 >= band.0 && m.target.0 <= band.1;
        s.push_str(&format!(
            "  {:4.2} {:4.2}  {}    {:6.3}   {:+.3}    {:+7.0}     {:5}{}\n",
            m.target.0,
            m.target.1,
            angle,
            m.reported.0,
            m.error_x(),
            m.error_x() * width_mm,
            m.samples,
            if inside { "  (calibrated)" } else { "" }
        ));
    }
    s
}

/// Mean absolute error and full-window capture rate, bucketed by how far the
/// eye had to rotate. The falloff tracks angle, not screen position, so this is
/// the summary that actually says something.
pub fn by_angle(measurements: &[Measurement], width_mm: f64, offset_x_mm: f64) -> String {
    let mut s = String::new();
    s.push_str("  eye rotation   mean error   full sample windows\n");
    for (lo, hi) in [(0.0, 15.0), (15.0, 28.0), (28.0, 90.0)] {
        let g: Vec<&Measurement> = measurements
            .iter()
            .filter(|m| {
                m.gaze_angle_deg(width_mm, offset_x_mm)
                    .is_some_and(|a| a.abs() >= lo && a.abs() < hi)
            })
            .collect();
        if g.is_empty() {
            continue;
        }
        let full = g
            .iter()
            .filter(|m| m.samples >= SAMPLE_TICKS as usize)
            .count();
        let usable: Vec<&&Measurement> = g.iter().filter(|m| m.samples > 0).collect();
        let mean = if usable.is_empty() {
            f64::NAN
        } else {
            usable.iter().map(|m| m.error_x().abs()).sum::<f64>() / usable.len() as f64
        };
        s.push_str(&format!(
            "  {lo:5.0}-{hi:3.0} deg    {:6.0} mm    {full:2}/{:<2}\n",
            mean * width_mm,
            g.len()
        ));
    }
    s
}

/// Mean absolute error of each eye's own gaze point against the fused one,
/// split by which way the user had to look.
///
/// The question this answers: is the fused point wrong because both eyes are
/// wrong, or because one eye is dragging the average? Only the second is
/// actionable, and the fused reading alone cannot tell them apart.
pub fn by_eye(measurements: &[Measurement], width_mm: f64, offset_x_mm: f64) -> String {
    let mut s = String::new();
    s.push_str("  looking      fused    left eye   right eye\n");
    for (lo, hi, label) in [
        (-90.0, -8.0, "left   "),
        (-8.0, 8.0, "centre "),
        (8.0, 90.0, "right  "),
    ] {
        let g: Vec<&Measurement> = measurements
            .iter()
            .filter(|m| m.samples > 0)
            .filter(|m| {
                m.gaze_angle_deg(width_mm, offset_x_mm)
                    .is_some_and(|a| a >= lo && a < hi)
            })
            .collect();
        if g.is_empty() {
            continue;
        }
        let mean = |v: Vec<f64>| -> String {
            if v.is_empty() {
                "    --".to_string()
            } else {
                format!("{:6.0}", v.iter().sum::<f64>() / v.len() as f64 * width_mm)
            }
        };
        s.push_str(&format!(
            "  {label}   {} mm  {} mm  {} mm\n",
            mean(g.iter().map(|m| m.error_x().abs()).collect()),
            mean(
                g.iter()
                    .filter_map(|m| m.eye_error_x(false))
                    .map(f64::abs)
                    .collect()
            ),
            mean(
                g.iter()
                    .filter_map(|m| m.eye_error_x(true))
                    .map(f64::abs)
                    .collect()
            ),
        ));
    }
    s
}

/// Fit `reported = gain x true + offset` over the range the device tracks
/// well, and report how much of the error is that single number.
///
/// This is the statistic that survived. A scale error means the reported gaze
/// spans more (or less) of the screen than the screen actually occupies, which
/// is what a display plane at the wrong depth or the wrong width produces —
/// and unlike a runtime correction, that is a config number the device is told
/// once. Fitted only inside `USABLE_GAZE_DEG`, since beyond it the device is
/// losing samples and its output is not a mapping of anything.
pub fn by_scale(measurements: &[Measurement], width_mm: f64, offset_x_mm: f64) -> String {
    let g: Vec<(f64, f64)> = measurements
        .iter()
        .filter(|m| m.samples >= SAMPLE_TICKS as usize)
        .filter(|m| {
            m.gaze_angle_deg(width_mm, offset_x_mm)
                .is_some_and(|a| a.abs() < tobii_config::USABLE_GAZE_DEG)
        })
        .map(|m| {
            (
                m.offset_x() * width_mm,
                m.reported.0.mul_add(width_mm, -0.5 * width_mm),
            )
        })
        .collect();
    if g.len() < 6 {
        return "  (too few well-sampled targets inside the working range to fit a scale)\n".into();
    }
    let n = g.len() as f64;
    let (mx, my) = (
        g.iter().map(|p| p.0).sum::<f64>() / n,
        g.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let var = g.iter().map(|p| (p.0 - mx).powi(2)).sum::<f64>();
    if var <= f64::EPSILON {
        return "  (targets do not span enough width to fit a scale)\n".into();
    }
    let gain = g.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>() / var;
    let off = my - gain * mx;

    let raw = g.iter().map(|p| (p.1 - p.0).abs()).sum::<f64>() / n;
    let residual = g
        .iter()
        .map(|p| (p.1 - (gain * p.0 + off)).abs())
        .sum::<f64>()
        / n;
    let share = if raw > 0.0 {
        100.0 * (1.0 - residual / raw)
    } else {
        0.0
    };
    format!(
        "  reported gaze is scaled {:+.1}% and shifted {:+.0} mm\n           inside the working range that accounts for {share:.0}% of the error \
         ({raw:.0} mm -> {residual:.0} mm without it)\n",
        (gain - 1.0) * 100.0,
        off,
    )
}

/// Run the measurement fullscreen: show each target in turn, record what the
/// device reports, then write a CSV and print the profile and diagnosis.
///
/// Deliberately a separate entry point (`tobii-gtk --accuracy`) rather than a
/// hub button: it is a diagnostic that wants the whole screen and a user who
/// knows to hold their fixation, not something to stumble into.
pub fn launch(
    app: &gtk::Application,
    state: std::sync::Arc<std::sync::Mutex<crate::device::DeviceState>>,
) -> gtk::ApplicationWindow {
    use gtk::glib;
    use gtk::prelude::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    let setup = tobii_config::load().ok().flatten();
    let width_mm = setup.as_ref().map_or(0.0, |s| s.width_mm);
    let height_mm = setup.as_ref().map_or(0.0, |s| s.height_mm);
    let offset_x_mm = setup.as_ref().map_or(0.0, |s| s.offset_x_mm);
    let eye_mode = state.lock().ok().and_then(|s| s.enabled_eye);
    let band = calibrated_band(width_mm, height_mm, &crate::calibrate_flow::FULL_7);

    let win = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Gaze accuracy check")
        .build();
    win.set_modal(true);
    win.fullscreen();

    let targets = targets();
    let index = Rc::new(std::cell::Cell::new(0usize));
    let ticks = Rc::new(std::cell::Cell::new(0u32));
    let samples: Rc<RefCell<Vec<(f64, f64)>>> = Rc::new(RefCell::new(Vec::new()));
    let eyes: Rc<RefCell<Vec<[f64; 3]>>> = Rc::new(RefCell::new(Vec::new()));
    /// Left-eye and right-eye gaze samples for the target being measured.
    type PerEyeSamples = Rc<RefCell<(Vec<(f64, f64)>, Vec<(f64, f64)>)>>;
    let per_eye: PerEyeSamples = Rc::new(RefCell::new((Vec::new(), Vec::new())));
    let results: Rc<RefCell<Vec<Measurement>>> = Rc::new(RefCell::new(Vec::new()));

    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    {
        let (index, ticks, targets) = (index.clone(), ticks.clone(), targets.clone());
        area.set_draw_func(move |_, cr, w, h| {
            cr.set_source_rgb(0.0, 0.0, 0.0);
            let _ = cr.paint();
            let Some(&(tx, ty)) = targets.get(index.get()) else {
                return;
            };
            let (cx, cy) = (tx * w as f64, ty * h as f64);
            // Solid while settling, ringed while sampling, so the user can see
            // when holding still actually matters.
            let sampling = ticks.get() >= SETTLE_TICKS;
            cr.set_source_rgb(0.35, 0.78, 0.78);
            cr.arc(cx, cy, 9.0, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
            if sampling {
                cr.set_line_width(2.0);
                cr.arc(cx, cy, 18.0, 0.0, std::f64::consts::TAU);
                let _ = cr.stroke();
            }
        });
    }
    win.set_child(Some(&area));

    let tick = {
        let (index, ticks, samples, eyes, per_eye, results) = (
            index.clone(),
            ticks.clone(),
            samples.clone(),
            eyes.clone(),
            per_eye.clone(),
            results.clone(),
        );
        let (win, area, targets) = (win.clone(), area.clone(), targets.clone());
        move || {
            let i = index.get();
            let Some(&target) = targets.get(i) else {
                return glib::ControlFlow::Break;
            };
            let t = ticks.get() + 1;
            ticks.set(t);

            if t > SETTLE_TICKS {
                if let Ok(s) = state.lock() {
                    if let Some(g) = &s.latest_gaze {
                        // Both eyes' validity gates the combined gaze point;
                        // a point computed from one eye is still usable, and
                        // dropping it would bias the profile toward whichever
                        // targets happen to keep both eyes in view.
                        let usable = (s.eye_view.is_some())
                            && (g.validity_l == 0 || g.validity_r == 0)
                            && g.has(tobii_protocol::gaze::present::GAZE_2D)
                            && g.gaze_point_2d[0] > -0.5;
                        if usable {
                            samples
                                .borrow_mut()
                                .push((g.gaze_point_2d[0], g.gaze_point_2d[1]));
                            // Whichever eyes are valid, midway between them.
                            // Captured alongside every gaze sample so the head
                            // position belongs to *this* target rather than to
                            // whatever the head was doing at some other time.
                            let mut acc = [0.0f64; 3];
                            let mut n = 0.0;
                            for (valid, origin) in [
                                (g.validity_l == 0, g.eye_origin_l_mm),
                                (g.validity_r == 0, g.eye_origin_r_mm),
                            ] {
                                if valid {
                                    for k in 0..3 {
                                        acc[k] += origin[k];
                                    }
                                    n += 1.0;
                                }
                            }
                            if n > 0.0 {
                                eyes.borrow_mut().push([acc[0] / n, acc[1] / n, acc[2] / n]);
                            }
                            // Per-eye gaze, gated on that eye's own validity —
                            // the device zeroes an eye's block when it loses it,
                            // and a (0,0) would read as a huge leftward error.
                            let mut pe = per_eye.borrow_mut();
                            if g.validity_l == 0 && g.has(tobii_protocol::gaze::present::GAZE_2D_L)
                            {
                                pe.0.push((g.gaze_point_2d_l[0], g.gaze_point_2d_l[1]));
                            }
                            if g.validity_r == 0 && g.has(tobii_protocol::gaze::present::GAZE_2D_R)
                            {
                                pe.1.push((g.gaze_point_2d_r[0], g.gaze_point_2d_r[1]));
                            }
                        }
                    }
                }
            }

            if t >= SETTLE_TICKS + SAMPLE_TICKS {
                let taken = samples.borrow().len();
                let reported = aggregate(&samples.borrow()).unwrap_or((0.0, 0.0));
                let eye_mm = mean_eye(&eyes.borrow());
                let (rl, rr) = {
                    let pe = per_eye.borrow();
                    (aggregate(&pe.0), aggregate(&pe.1))
                };
                results.borrow_mut().push(Measurement {
                    target,
                    reported,
                    samples: taken,
                    eye_mm,
                    reported_l: rl,
                    reported_r: rr,
                });
                samples.borrow_mut().clear();
                eyes.borrow_mut().clear();
                per_eye.borrow_mut().0.clear();
                per_eye.borrow_mut().1.clear();
                ticks.set(0);
                index.set(i + 1);
                if i + 1 >= targets.len() {
                    finish(&results.borrow(), band, width_mm, offset_x_mm, eye_mode);
                    win.close();
                    return glib::ControlFlow::Break;
                }
            }
            area.queue_draw();
            glib::ControlFlow::Continue
        }
    };
    glib::timeout_add_local(std::time::Duration::from_millis(33), tick);

    let esc = gtk::EventControllerKey::new();
    {
        let (win, results) = (win.clone(), results.clone());
        esc.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                // Report whatever was gathered — a half-finished sweep of the
                // middle is still worth more than nothing.
                finish(&results.borrow(), band, width_mm, offset_x_mm, eye_mode);
                win.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    win.add_controller(esc);

    win.present();
    win
}

/// Write the CSV and print the profile + diagnosis.
fn finish(
    measurements: &[Measurement],
    band: (f64, f64),
    width_mm: f64,
    offset_x_mm: f64,
    eye_mode: Option<tobii_protocol::EnabledEye>,
) {
    if measurements.is_empty() {
        eprintln!("accuracy check: no targets completed");
        return;
    }
    let d = diagnose(measurements, band);
    println!("\n=== gaze accuracy ===");
    // Stated first, and loudly when it is not Both. A monocular run is not
    // comparable to a binocular one: the per-eye gaze points straddle the
    // target by ~110mm, so with one eye the device reports that eye's half of
    // the straddle as a uniform offset across the whole screen. Comparing the
    // two silently reads as "everything suddenly got six times worse".
    match eye_mode {
        Some(tobii_protocol::EnabledEye::Both) | None => {}
        Some(e) => {
            println!(
                "!! the tracker is in {e:?}-eye-only mode. These numbers are NOT comparable\n                 !! to a both-eyes run, and a calibration made with both eyes leaves that\n                 !! eye's share of the straddle uncorrected across the whole screen.\n                 !! Set both eyes in the hub, or recalibrate in this mode.\n"
            );
        }
    }
    print!("{}", report(measurements, band, width_mm, offset_x_mm));
    println!();
    print!("{}", by_angle(measurements, width_mm, offset_x_mm));
    println!();
    print!("{}", by_eye(measurements, width_mm, offset_x_mm));
    println!();
    print!("{}", by_scale(measurements, width_mm, offset_x_mm));

    // Where the head actually was, so nobody has to infer it later.
    let eyes: Vec<[f64; 3]> = measurements.iter().filter_map(|m| m.eye_mm).collect();
    if let Some(e) = mean_eye(&eyes) {
        println!(
            "\nyour head, averaged over the run: {:.0} mm {} of screen centre, {:.0} mm back",
            (e[0] - offset_x_mm).abs(),
            if e[0] - offset_x_mm < 0.0 {
                "left"
            } else {
                "right"
            },
            e[2]
        );
    }
    println!(
        "calibrated band: {:.2}..{:.2} of screen width",
        band.0, band.1
    );
    println!(
        "error per unit offset: {:.3} inside, {:.3} outside",
        d.inner_gain, d.outer_gain
    );
    println!(
        "worst error: {:.1}% of width ({:.0} mm)",
        d.worst_error * 100.0,
        d.worst_error * width_mm
    );
    println!("\nverdict: {:?}\n  {}", d.cause, d.detail);

    let path = tobii_config::config_path().with_file_name("accuracy.csv");
    let mut csv = String::from(
        "target_x,target_y,reported_x,reported_y,error_x,samples,eye_x_mm,eye_y_mm,eye_z_mm,gaze_angle_deg,reported_l_x,reported_r_x\n",
    );
    for m in measurements {
        let (ex, ey, ez) = match m.eye_mm {
            Some(e) => (e[0].to_string(), e[1].to_string(), e[2].to_string()),
            None => (String::new(), String::new(), String::new()),
        };
        csv.push_str(&format!(
            "{},{},{},{},{},{},{ex},{ey},{ez},{}\n",
            m.target.0,
            m.target.1,
            m.reported.0,
            m.reported.1,
            m.error_x(),
            m.samples,
            m.gaze_angle_deg(width_mm, offset_x_mm)
                .map(|a| a.to_string())
                .unwrap_or_default(),
        ));
        csv.pop();
        csv.push_str(&format!(
            ",{},{}\n",
            m.reported_l.map(|p| p.0.to_string()).unwrap_or_default(),
            m.reported_r.map(|p| p.0.to_string()).unwrap_or_default(),
        ));
    }
    match std::fs::write(&path, csv) {
        Ok(()) => println!("raw data: {}", path.display()),
        Err(e) => eprintln!("could not write {}: {e}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(tx: f64, err: f64) -> Measurement {
        Measurement {
            target: (tx, 0.5),
            reported: (tx + err, 0.5),
            samples: 30,
            eye_mm: None,
            reported_l: None,
            reported_r: None,
        }
    }

    #[test]
    fn targets_span_the_literal_screen_edges() {
        let t = targets();
        let xs: Vec<f64> = t.iter().map(|p| p.0).collect();
        assert!(xs.iter().cloned().fold(f64::INFINITY, f64::min) <= 0.02);
        assert!(xs.iter().cloned().fold(0.0, f64::max) >= 0.98);
        // The centre comes first so an aborted run still has a usable middle.
        assert_eq!(t[0].0, 0.5);
    }

    #[test]
    fn no_target_repeats_the_previous_position() {
        let t = targets();
        for w in t.windows(2) {
            assert_ne!(w[0], w[1], "a repeated position invites a stale fixation");
        }
    }

    #[test]
    fn aggregate_takes_the_median_so_one_blink_cannot_move_it() {
        let mut s: Vec<(f64, f64)> = (0..20).map(|_| (0.5, 0.5)).collect();
        s.push((9.0, -9.0)); // a glance away
        assert_eq!(aggregate(&s), Some((0.5, 0.5)));
        assert_eq!(aggregate(&[]), None);
    }

    #[test]
    fn a_constant_gain_reads_as_plane_geometry() {
        // Error strictly proportional to distance from centre, inside and out.
        let ms: Vec<Measurement> = [0.02, 0.2, 0.35, 0.5, 0.65, 0.8, 0.98]
            .iter()
            .map(|&x| m(x, (x - 0.5) * 0.12))
            .collect();
        let d = diagnose(&ms, (0.30, 0.70));
        assert_eq!(d.cause, Cause::PlaneGeometry, "{}", d.detail);
    }

    #[test]
    fn error_that_climbs_toward_the_edges_reads_as_curvature() {
        // Cubic in offset: small mid-screen, large at the edges — and a real
        // gain inside the band too, which is what separates this from a pure
        // coverage gap.
        let ms: Vec<Measurement> = [0.02, 0.2, 0.35, 0.5, 0.65, 0.8, 0.98]
            .iter()
            .map(|&x| {
                let o = x - 0.5;
                m(x, 0.05 * o + 2.4 * o * o * o)
            })
            .collect();
        let d = diagnose(&ms, (0.30, 0.70));
        assert_eq!(d.cause, Cause::Curvature, "{}", d.detail);
    }

    #[test]
    fn accuracy_that_stops_at_the_calibration_boundary_reads_as_coverage() {
        // Flat and correct inside the band; badly wrong outside it.
        let ms: Vec<Measurement> = [0.02, 0.2, 0.35, 0.5, 0.65, 0.8, 0.98]
            .iter()
            .map(|&x| {
                let err = if (0.30..=0.70).contains(&x) {
                    0.0
                } else {
                    (x - 0.5) * 0.30
                };
                m(x, err)
            })
            .collect();
        let d = diagnose(&ms, (0.30, 0.70));
        assert_eq!(d.cause, Cause::CalibrationCoverage, "{}", d.detail);
    }

    #[test]
    fn a_device_that_stops_answering_at_the_edges_outranks_any_geometry() {
        // The shape that fooled the first version of this: wild errors at the
        // edges read as "curvature", when the real story is that the device
        // returned barely any data out there. Capture rate has to win.
        let mut ms: Vec<Measurement> = [0.35, 0.5, 0.65]
            .iter()
            .map(|&x| m(x, (x - 0.5) * 0.1))
            .collect();
        for x in [0.02, 0.1, 0.9, 0.98] {
            ms.push(Measurement {
                target: (x, 0.5),
                reported: (x + (x - 0.5) * 0.4, 0.5),
                samples: 8, // device managed a fraction of the window
                eye_mm: None,
                reported_l: None,
                reported_r: None,
            });
        }
        let d = diagnose(&ms, (0.30, 0.70));
        assert_eq!(d.cause, Cause::DeviceAngularLimit, "{}", d.detail);
    }

    #[test]
    fn too_few_points_on_one_side_refuses_to_name_a_cause() {
        // Two inner points cannot distinguish a scale error from a shape error,
        // however tidy the ratio between them looks.
        let ms = vec![m(0.35, -0.03), m(0.65, 0.001), m(0.1, -0.1), m(0.9, 0.1)];
        let d = diagnose(&ms, (0.30, 0.70));
        assert_eq!(d.cause, Cause::Inconclusive, "{}", d.detail);
    }

    #[test]
    fn a_clean_profile_is_not_talked_into_a_diagnosis() {
        let ms: Vec<Measurement> = [0.02, 0.2, 0.5, 0.8, 0.98]
            .iter()
            .map(|&x| m(x, 0.002))
            .collect();
        let d = diagnose(&ms, (0.30, 0.70));
        assert_eq!(d.cause, Cause::NoneApparent, "{}", d.detail);
    }

    #[test]
    fn targets_with_no_gaze_data_are_counted_not_averaged_in() {
        let ms = vec![
            m(0.5, 0.0),
            Measurement {
                target: (0.98, 0.5),
                reported: (0.0, 0.0),
                samples: 0,
                eye_mm: None,
                reported_l: None,
                reported_r: None,
            },
        ];
        let d = diagnose(&ms, (0.30, 0.70));
        assert_eq!(d.dead_targets, 1);
        // The (0,0) placeholder would be a -0.98 error if it were counted.
        assert!(d.worst_error < 0.01, "worst {}", d.worst_error);
    }

    /// A real sweep, captured 2026-08-10 on a Samsung Odyssey G93SC (49",
    /// 32:9, 1193 x 336 mm, 1800R) with a calibrated ET5. `(x, y, reported_x,
    /// samples)`.
    ///
    /// Kept because it is the only ground truth this project has for how the
    /// ET5 behaves on a screen far wider than it was designed for, and because
    /// the first version of [`diagnose`] read it as `Curvature` — confidently,
    /// and wrongly. A flat-plane-versus-1800R model accounts for only about
    /// half the measured error and gets the sign wrong at x = 0.80. What the
    /// numbers actually show is the device running out of resolvable eye
    /// rotation: full sample windows for 93% of mid-screen targets against 42%
    /// at the edges.
    ///
    /// The head position was **not** recorded (this run predates that), and the
    /// first reading of it borrowed an eye position from an unrelated capture,
    /// which made a symmetric falloff look like an asymmetric seating problem.
    /// The user reported sitting centred; at ~795 mm that puts both screen
    /// edges at ±36° of eye rotation, and the |error| at ±35.8° comes out at
    /// 167 mm and 176 mm — symmetric, as a physical limit should be.
    const REAL_SWEEP: &[(f64, f64, f64, usize)] = &[
        (0.50, 0.50, 0.4915, 30),
        (0.50, 0.15, 0.4885, 30),
        (0.50, 0.85, 0.4910, 30),
        (0.35, 0.15, 0.3185, 30),
        (0.35, 0.85, 0.3191, 30),
        (0.35, 0.50, 0.3216, 30),
        (0.65, 0.85, 0.6521, 30),
        (0.65, 0.50, 0.6535, 30),
        (0.65, 0.15, 0.6397, 26),
        (0.20, 0.50, 0.1506, 30),
        (0.20, 0.15, 0.1746, 30),
        (0.20, 0.85, 0.0829, 30),
        (0.80, 0.15, 0.7966, 30),
        (0.80, 0.85, 0.7428, 30),
        (0.80, 0.50, 0.7621, 30),
        (0.10, 0.85, 0.0634, 30),
        (0.10, 0.50, -0.0153, 30),
        (0.10, 0.15, 0.0000, 0),
        (0.90, 0.50, 1.0217, 29),
        (0.90, 0.15, 0.0000, 0),
        (0.90, 0.85, 1.0347, 30),
        (0.02, 0.15, 0.0000, 0),
        (0.02, 0.85, -0.1442, 30),
        (0.02, 0.50, -0.0957, 30),
        (0.98, 0.85, 1.0902, 7),
        (0.98, 0.50, 1.0930, 20),
        (0.98, 0.15, 1.1807, 18),
    ];

    fn real_sweep() -> Vec<Measurement> {
        REAL_SWEEP
            .iter()
            .map(|&(tx, ty, rx, n)| Measurement {
                target: (tx, ty),
                reported: (rx, ty),
                samples: n,
                // This capture predates per-target eye recording — which is
                // precisely why that recording now exists.
                eye_mm: None,
                reported_l: None,
                reported_r: None,
            })
            .collect()
    }

    #[test]
    fn the_real_sweep_reads_as_the_device_running_out_of_angle() {
        let band = calibrated_band(1193.0, 336.0, &crate::calibrate_flow::FULL_7);
        let d = diagnose(&real_sweep(), band);
        assert_eq!(
            d.cause,
            Cause::DeviceAngularLimit,
            "curvature was the seductive reading here and it explains only half \
             the error: {}",
            d.detail
        );
    }

    #[test]
    fn the_real_sweep_is_accurate_across_the_middle_of_the_screen() {
        // Whatever is wrong at the edges, the middle is not the problem, and a
        // future change must not trade the middle away to flatten the edges.
        let worst_mid = real_sweep()
            .iter()
            .filter(|m| m.samples > 0 && m.offset_x().abs() <= 0.2)
            .map(|m| m.error_x().abs())
            .fold(0.0f64, f64::max);
        assert!(
            worst_mid < 0.04,
            "middle 40% of the screen drifted to {:.3} of screen width",
            worst_mid
        );
    }

    #[test]
    fn the_calibrated_band_is_the_real_one_not_the_full_screen() {
        // The user's 1193x336 panel: capping confines calibration to 600mm,
        // and FULL_7's own 0.1..0.9 inset narrows it further.
        let pts = crate::calibrate_flow::FULL_7;
        let (lo, hi) = calibrated_band(1193.0, 336.0, &pts);
        assert!((lo - 0.299).abs() < 0.005, "lo {lo}");
        assert!((hi - 0.701).abs() < 0.005, "hi {hi}");

        // A small screen is not capped, so the band is the point set's own span.
        let (lo, hi) = calibrated_band(500.0, 300.0, &pts);
        assert!(
            (lo - 0.1).abs() < 1e-9 && (hi - 0.9).abs() < 1e-9,
            "{lo} {hi}"
        );
    }
}
