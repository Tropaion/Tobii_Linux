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
    let xs = [0.5, 0.35, 0.65, 0.2, 0.8, 0.1, 0.9, 0.02, 0.98];
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
    /// Nothing stands out above the measurement noise.
    NoneApparent,
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

    let (cause, detail) = if worst_error <= NOISE_FLOOR && dead_targets == 0 {
        (
            Cause::NoneApparent,
            format!(
                "worst error {:.1}% of screen width — at the noise floor",
                worst_error * 100.0
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
pub fn report(measurements: &[Measurement], band: (f64, f64), width_mm: f64) -> String {
    let mut s = String::new();
    s.push_str("  target x   reported   error      error in mm   samples\n");
    let mut rows: Vec<&Measurement> = measurements.iter().collect();
    rows.sort_by(|a, b| a.target.0.partial_cmp(&b.target.0).unwrap());
    for m in rows {
        if m.samples == 0 {
            s.push_str(&format!(
                "   {:5.2}      —          —          —             0   <-- no gaze data\n",
                m.target.0
            ));
            continue;
        }
        let inside = m.target.0 >= band.0 && m.target.0 <= band.1;
        s.push_str(&format!(
            "   {:5.2}     {:6.3}    {:+.3}     {:+7.0}       {:5}{}\n",
            m.target.0,
            m.reported.0,
            m.error_x(),
            m.error_x() * width_mm,
            m.samples,
            if inside { "   (calibrated)" } else { "" }
        ));
    }
    s
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
        let (index, ticks, samples, results) = (
            index.clone(),
            ticks.clone(),
            samples.clone(),
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
                        }
                    }
                }
            }

            if t >= SETTLE_TICKS + SAMPLE_TICKS {
                let taken = samples.borrow().len();
                let reported = aggregate(&samples.borrow()).unwrap_or((0.0, 0.0));
                results.borrow_mut().push(Measurement {
                    target,
                    reported,
                    samples: taken,
                });
                samples.borrow_mut().clear();
                ticks.set(0);
                index.set(i + 1);
                if i + 1 >= targets.len() {
                    finish(&results.borrow(), band, width_mm);
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
                finish(&results.borrow(), band, width_mm);
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
fn finish(measurements: &[Measurement], band: (f64, f64), width_mm: f64) {
    if measurements.is_empty() {
        eprintln!("accuracy check: no targets completed");
        return;
    }
    let d = diagnose(measurements, band);
    println!("\n=== gaze accuracy ===");
    print!("{}", report(measurements, band, width_mm));
    println!(
        "\ncalibrated band: {:.2}..{:.2} of screen width",
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
    let mut csv = String::from("target_x,target_y,reported_x,reported_y,error_x,samples\n");
    for m in measurements {
        csv.push_str(&format!(
            "{},{},{},{},{},{}\n",
            m.target.0,
            m.target.1,
            m.reported.0,
            m.reported.1,
            m.error_x(),
            m.samples
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
            },
        ];
        let d = diagnose(&ms, (0.30, 0.70));
        assert_eq!(d.dead_targets, 1);
        // The (0,0) placeholder would be a -0.98 error if it were counted.
        assert!(d.worst_error < 0.01, "worst {}", d.worst_error);
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
