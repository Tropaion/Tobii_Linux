//! Fullscreen "fine-tune gaze alignment" flow: correct a constant offset
//! between where the user looks and where the tracker thinks they look, by eye
//! instead of with a ruler.
//!
//! The dominant source of that offset is `offset_y_mm` — the height of the
//! screen's bottom edge above the tracker — which nobody measures and which the
//! defaults guess at. An error there shifts *every* gaze point vertically by
//! roughly the same amount, so it shows up as "the dot is always 3 cm too high".
//! `offset_x_mm` has the same character horizontally.
//!
//! The flow is: look at a cross at the screen centre, freeze the (averaged)
//! reported gaze, then drag that frozen marker onto the cross. The drag is the
//! measurement — [`corrected_offsets`] turns it into millimetres.
//!
//! The live dot here deliberately does **not** get `correct_gaze_x` applied
//! (unlike `overlay.rs`): the solver works in the device's own flat-plane
//! normalized frame, so the captured point must be in that frame too. The
//! curvature correction is near-zero at the screen centre — which is exactly
//! where the target cross sits — so this costs nothing in practice.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::{
    cairo, Align, Application, ApplicationWindow, Button, DrawingArea, GestureDrag, Label,
    Orientation, Overlay,
};

use crate::device::{ConnStatus, DeviceCommand, DeviceState};
use crate::{add_escape_to_close, screen_height};
use tobii_config::DisplaySetup;
use tobii_protocol::gaze::present;

/// The target cross: the exact centre of the screen, in normalized coordinates.
pub const TARGET: [f64; 2] = [0.5, 0.5];

/// Valid samples averaged into the captured point: ~0.5 s at the 33 ms tick.
const CAPTURE_TICKS: u32 = 15;

/// New `(offset_x_mm, offset_y_mm)` that move the reported gaze from `captured`
/// onto `target`. Both points are normalized display coordinates (origin at the
/// top-left corner, +y **down**) — the frame the device reports gaze in.
///
/// Derivation, read straight off [`DisplaySetup::to_corners`]:
///
/// * `TL.x = offset_x - width/2`, and the reported horizontal coordinate is
///   `u = (x_hit - TL.x) / width`. So `∂u/∂offset_x = -1/width`: **increasing**
///   `offset_x` **decreases** `u`, i.e. drags the reported dot left. To move the
///   dot right by `d[0]`, `offset_x` must go *down* by `d[0] * width`.
/// * `TL.y = offset_y + height·cos(tilt)`, `BL.y = offset_y`, and `v` is
///   measured downward from `TL`, so `v = (TL.y - y_hit) / height` for an
///   upright screen. `∂v/∂offset_y = +1/height`: raising the plane moves the
///   reported dot *down*. To move the dot down by `d[1]`, `offset_y` goes *up*
///   by `d[1] * height`.
///
/// Exact for an untilted screen (translating the plane along its own surface
/// changes nothing but the normalization); first-order for a tilted one, where
/// shifting the plane also moves where the gaze ray pierces it. See the tests.
///
/// Non-finite inputs (or a degenerate setup) return the setup's current offsets
/// unchanged rather than poisoning the saved configuration with NaN.
pub fn corrected_offsets(captured: [f64; 2], target: [f64; 2], setup: &DisplaySetup) -> (f64, f64) {
    let current = (setup.offset_x_mm, setup.offset_y_mm);
    let sane = captured.iter().chain(target.iter()).all(|v| v.is_finite())
        && setup.width_mm.is_finite()
        && setup.height_mm.is_finite()
        && current.0.is_finite()
        && current.1.is_finite();
    if !sane {
        return current;
    }
    let dx = target[0] - captured[0];
    let dy = target[1] - captured[1];
    let out = (
        current.0 - dx * setup.width_mm,
        current.1 + dy * setup.height_mm,
    );
    if out.0.is_finite() && out.1.is_finite() {
        out
    } else {
        current
    }
}

/// UI state of the flow.
enum Phase {
    /// Showing the live gaze dot, waiting for Space / Capture.
    Live,
    /// Averaging valid samples into a captured point.
    Capturing { ticks: u32, sum: [f64; 2], n: u32 },
    /// The captured marker is draggable; dragging it onto the cross is the fix.
    Adjust {
        captured: [f64; 2],
        marker: [f64; 2],
    },
}

/// Dark backdrop, the target cross, and whichever dots the phase calls for.
fn draw_scene(cr: &cairo::Context, w: i32, h: i32, phase: &Phase, live: Option<(f64, f64)>) {
    let (w, h) = (w as f64, h as f64);
    cr.set_source_rgb(0.08, 0.09, 0.11);
    let _ = cr.paint();

    // Target cross, dead centre.
    let (cx, cy) = (TARGET[0] * w, TARGET[1] * h);
    cr.set_source_rgb(0.92, 0.94, 0.96);
    cr.set_line_width(2.0);
    let arm = 26.0;
    cr.move_to(cx - arm, cy);
    cr.line_to(cx + arm, cy);
    cr.move_to(cx, cy - arm);
    cr.line_to(cx, cy + arm);
    let _ = cr.stroke();
    cr.arc(cx, cy, 5.0, 0.0, std::f64::consts::TAU);
    let _ = cr.fill();

    match phase {
        Phase::Live | Phase::Capturing { .. } => {
            if let Some((gx, gy)) = live {
                let (x, y) = (gx.clamp(0.0, 1.0) * w, gy.clamp(0.0, 1.0) * h);
                cr.arc(x, y, 18.0, 0.0, std::f64::consts::TAU);
                cr.set_source_rgba(0.15, 0.85, 0.85, 0.28);
                let _ = cr.fill_preserve();
                cr.set_source_rgba(0.15, 0.85, 0.85, 0.95);
                cr.set_line_width(3.0);
                let _ = cr.stroke();
            }
        }
        Phase::Adjust { captured, marker } => {
            let (ox, oy) = (captured[0] * w, captured[1] * h);
            let (mx, my) = (marker[0] * w, marker[1] * h);
            // Faint ghost of where the tracker actually reported the gaze, so
            // the size of the error stays visible while dragging.
            cr.set_source_rgba(0.15, 0.85, 0.85, 0.30);
            cr.set_line_width(1.5);
            cr.arc(ox, oy, 18.0, 0.0, std::f64::consts::TAU);
            let _ = cr.stroke();
            cr.set_dash(&[5.0, 4.0], 0.0);
            cr.move_to(ox, oy);
            cr.line_to(mx, my);
            let _ = cr.stroke();
            cr.set_dash(&[], 0.0);
            // The draggable marker.
            cr.arc(mx, my, 20.0, 0.0, std::f64::consts::TAU);
            cr.set_source_rgba(0.95, 0.72, 0.20, 0.35);
            let _ = cr.fill_preserve();
            cr.set_source_rgb(0.95, 0.72, 0.20);
            cr.set_line_width(3.0);
            let _ = cr.stroke();
        }
    }
}

/// Open the fullscreen fine-tune flow. Returns the window so the caller can
/// re-enable its launch button when it closes.
pub fn launch(
    app: &Application,
    state: Arc<Mutex<DeviceState>>,
    cmd_tx: Sender<DeviceCommand>,
) -> ApplicationWindow {
    // Copy semantics (DisplaySetup is Copy), so no borrow can outlive a handler.
    let setup = Rc::new(Cell::new(
        tobii_config::load()
            .ok()
            .flatten()
            .unwrap_or_else(crate::setup_flow::default_setup),
    ));

    let win = ApplicationWindow::builder()
        .application(app)
        .title("Fine-tune gaze alignment")
        .build();
    win.set_modal(true);
    win.fullscreen();

    let phase = Rc::new(RefCell::new(Phase::Live));
    let live: Rc<Cell<Option<(f64, f64)>>> = Rc::new(Cell::new(None));

    let area = DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    {
        let phase = phase.clone();
        let live = live.clone();
        area.set_draw_func(move |_, cr, w, h| {
            draw_scene(cr, w, h, &phase.borrow(), live.get());
        });
    }

    let instr = Label::new(None);
    instr.add_css_class("app-title");
    instr.set_halign(Align::Center);
    instr.set_justify(gtk::Justification::Center);
    instr.set_wrap(true);
    instr.set_max_width_chars(60);

    let status = Label::new(None);
    status.add_css_class("section-desc");
    status.set_halign(Align::Center);
    status.set_justify(gtk::Justification::Center);
    status.set_wrap(true);
    status.set_max_width_chars(70);

    let capture_btn = Button::with_label("Capture");
    let apply_btn = Button::with_label("Apply & save");
    let redo_btn = Button::with_label("Try again");
    let cancel_btn = Button::with_label("Cancel");
    let buttons = gtk::Box::new(Orientation::Horizontal, 10);
    buttons.set_halign(Align::Center);
    buttons.append(&capture_btn);
    buttons.append(&apply_btn);
    buttons.append(&redo_btn);
    buttons.append(&cancel_btn);

    let header = gtk::Box::new(Orientation::Vertical, 14);
    header.set_halign(Align::Center);
    header.set_valign(Align::Start);
    header.set_margin_top((screen_height() as f64 * 0.16) as i32);
    header.append(&instr);
    header.append(&status);
    header.append(&buttons);

    let overlay = Overlay::new();
    overlay.set_child(Some(&area));
    overlay.add_overlay(&header);
    win.set_child(Some(&overlay));

    // Reflect the phase in the instruction, the readout and which buttons show.
    let refresh_ui: Rc<dyn Fn()> = {
        let phase = phase.clone();
        let setup = setup.clone();
        let instr = instr.clone();
        let status = status.clone();
        let capture_btn = capture_btn.clone();
        let apply_btn = apply_btn.clone();
        let redo_btn = redo_btn.clone();
        let area = area.clone();
        Rc::new(move || {
            // Bind everything out of the borrow before touching widgets: this
            // codebase has shipped a double-borrow panic before (c8db2c7).
            let snapshot = match &*phase.borrow() {
                Phase::Live => None,
                Phase::Capturing { .. } => Some(None),
                Phase::Adjust { captured, marker } => Some(Some((*captured, *marker))),
            };
            match snapshot {
                None => {
                    instr.set_text(
                        "Look at the cross in the middle of the screen, then press Space.",
                    );
                    capture_btn.set_visible(true);
                    apply_btn.set_visible(false);
                    redo_btn.set_visible(false);
                    if !status.has_css_class("section-warn") {
                        status.set_text(
                            "The teal dot is where the tracker thinks you are looking. \
                             Hold your gaze on the cross while capturing.",
                        );
                    }
                }
                Some(None) => {
                    instr.set_text("Hold still — capturing…");
                    capture_btn.set_visible(false);
                    apply_btn.set_visible(false);
                    redo_btn.set_visible(false);
                }
                Some(Some((captured, marker))) => {
                    let s = setup.get();
                    let (nx, ny) = corrected_offsets(captured, marker, &s);
                    instr.set_text("Now drag the orange dot onto the cross.");
                    status.remove_css_class("section-warn");
                    status.set_text(&format!(
                        "Correction so far: horizontal {:+.0} mm, height {:+.0} mm.\n\
                         Screen centre {:.0} mm left/right of the tracker, \
                         bottom edge {:.0} mm above it (was {:.0} / {:.0}).",
                        nx - s.offset_x_mm,
                        ny - s.offset_y_mm,
                        nx,
                        ny,
                        s.offset_x_mm,
                        s.offset_y_mm,
                    ));
                    capture_btn.set_visible(false);
                    apply_btn.set_visible(true);
                    redo_btn.set_visible(true);
                }
            }
            area.queue_draw();
        })
    };

    // Space / Capture -> start averaging. Only meaningful from the live phase.
    let begin_capture: Rc<dyn Fn()> = {
        let phase = phase.clone();
        let status = status.clone();
        let refresh_ui = refresh_ui.clone();
        Rc::new(move || {
            let in_live = matches!(&*phase.borrow(), Phase::Live); // borrow dropped
            if !in_live {
                return;
            }
            status.remove_css_class("section-warn");
            *phase.borrow_mut() = Phase::Capturing {
                ticks: 0,
                sum: [0.0; 2],
                n: 0,
            };
            refresh_ui();
        })
    };
    {
        let begin_capture = begin_capture.clone();
        capture_btn.connect_clicked(move |_| begin_capture());
    }
    {
        let begin_capture = begin_capture.clone();
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::space {
                begin_capture();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        win.add_controller(keys);
    }
    {
        let phase = phase.clone();
        let refresh_ui = refresh_ui.clone();
        redo_btn.connect_clicked(move |_| {
            *phase.borrow_mut() = Phase::Live;
            refresh_ui();
        });
    }

    // Drag the captured marker. Same shape as setup_flow's line drag: snapshot
    // the starting position on begin, offset from it on every update.
    let drag = GestureDrag::new();
    let marker0 = Rc::new(Cell::new(TARGET));
    {
        let phase = phase.clone();
        let marker0 = marker0.clone();
        drag.connect_drag_begin(move |_, _, _| {
            if let Phase::Adjust { marker, .. } = &*phase.borrow() {
                marker0.set(*marker);
            }
        });
    }
    {
        let phase = phase.clone();
        let area = area.clone();
        let marker0 = marker0.clone();
        let refresh_ui = refresh_ui.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            let w = area.width().max(1) as f64;
            let h = area.height().max(1) as f64;
            let m0 = marker0.get();
            {
                let mut ph = phase.borrow_mut();
                if let Phase::Adjust { marker, .. } = &mut *ph {
                    *marker = [
                        (m0[0] + dx / w).clamp(0.0, 1.0),
                        (m0[1] + dy / h).clamp(0.0, 1.0),
                    ];
                } else {
                    return;
                }
            } // borrow released before refresh_ui re-reads the phase
            refresh_ui();
        });
    }
    area.add_controller(drag);

    // Apply: persist + push the new plane to the device, then return to the hub.
    {
        let phase = phase.clone();
        let setup = setup.clone();
        let status = status.clone();
        let win = win.clone();
        let cmd_tx = cmd_tx.clone();
        apply_btn.connect_clicked(move |_| {
            let pts = match &*phase.borrow() {
                Phase::Adjust { captured, marker } => Some((*captured, *marker)),
                _ => None,
            }; // borrow released
            let Some((captured, marker)) = pts else {
                return;
            };
            let mut s = setup.get();
            let (nx, ny) = corrected_offsets(captured, marker, &s);
            s.offset_x_mm = nx;
            s.offset_y_mm = ny;
            setup.set(s);
            let saved = tobii_config::save(&s);
            let _ = cmd_tx.send(DeviceCommand::SetDisplayArea(s.to_corners()));
            // Same contract as setup_flow: a failed save still reaches the
            // device, but will not survive a restart — say so rather than
            // closing as if everything worked.
            if let Err(e) = saved {
                status.add_css_class("section-warn");
                status.set_text(&format!(
                    "Applied to the tracker, but saving the configuration failed: {e}. \
                     The correction will be lost when the tracker reconnects."
                ));
                return;
            }
            win.close();
        });
    }
    {
        let win = win.clone();
        cancel_btn.connect_clicked(move |_| win.close());
    }

    add_escape_to_close(&win);

    // Retire the tick when the window goes away (button, Esc or the compositor).
    let closed = Rc::new(Cell::new(false));
    {
        let closed = closed.clone();
        win.connect_close_request(move |_| {
            closed.set(true);
            glib::Propagation::Proceed
        });
    }

    // ~30 fps: pull the live gaze, advance the capture average.
    {
        let phase = phase.clone();
        let live = live.clone();
        let status = status.clone();
        let refresh_ui = refresh_ui.clone();
        let area = area.clone();
        glib::timeout_add_local(Duration::from_millis(33), move || {
            if closed.get() {
                return glib::ControlFlow::Break;
            }
            let snap = state.lock().unwrap().clone();
            // Same validity gate as overlay.rs: the present bit alone is not
            // enough, the device publishes stale/zero points for untracked eyes.
            let g = if matches!(snap.status, ConnStatus::Connected) {
                snap.latest_gaze.as_ref().and_then(|s| {
                    (s.has(present::GAZE_2D) && s.validity_l == 0)
                        .then_some((s.gaze_point_2d[0], s.gaze_point_2d[1]))
                })
            } else {
                None
            };
            live.set(g);

            // Advance the capture window, then act on the outcome outside the
            // borrow (refresh_ui reads the phase again).
            let mut finished: Option<Option<[f64; 2]>> = None;
            {
                let mut ph = phase.borrow_mut();
                if let Phase::Capturing { ticks, sum, n } = &mut *ph {
                    if let Some((gx, gy)) = g {
                        sum[0] += gx;
                        sum[1] += gy;
                        *n += 1;
                    }
                    *ticks += 1;
                    if *ticks >= CAPTURE_TICKS {
                        finished = Some(if *n > 0 {
                            Some([sum[0] / *n as f64, sum[1] / *n as f64])
                        } else {
                            None
                        });
                    }
                }
            }
            match finished {
                Some(Some(captured)) => {
                    *phase.borrow_mut() = Phase::Adjust {
                        captured,
                        marker: captured,
                    };
                    refresh_ui();
                }
                Some(None) => {
                    // Nothing valid arrived: say so and stay in the live phase
                    // rather than freezing a marker on a point we never saw.
                    *phase.borrow_mut() = Phase::Live;
                    status.add_css_class("section-warn");
                    status.set_text(
                        "No gaze was detected while capturing. Check that the tracker is \
                         connected and that your eyes are inside its view, then try again.",
                    );
                    refresh_ui();
                }
                None => area.queue_draw(),
            }
            glib::ControlFlow::Continue
        });
    }

    refresh_ui();
    win.present();
    win
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> DisplaySetup {
        DisplaySetup {
            width_mm: 1171.0,
            height_mm: 336.0,
            tilt_deg: 0.0,
            offset_x_mm: 0.0,
            offset_y_mm: 40.0,
            offset_z_mm: 0.0,
            curvature_radius_mm: 0.0,
        }
    }

    fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }
    fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }
    fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    /// The point on `setup`'s screen at normalized `(u, v)` (v measured down
    /// from the top-left corner) — built from the real `to_corners`.
    fn point_at(setup: &DisplaySetup, u: f64, v: f64) -> [f64; 3] {
        let c = setup.to_corners();
        let ex = sub(c.tr, c.tl);
        let ey = sub(c.bl, c.tl);
        [
            c.tl[0] + u * ex[0] + v * ey[0],
            c.tl[1] + u * ex[1] + v * ey[1],
            c.tl[2] + u * ex[2] + v * ey[2],
        ]
    }

    /// What the device WOULD report for someone at `eye` looking at the world
    /// point `hit`, if it has been told about `setup`'s (possibly wrong) plane:
    /// intersect the gaze ray with that plane and normalize against its corners.
    fn reported(eye: [f64; 3], hit: [f64; 3], setup: &DisplaySetup) -> [f64; 2] {
        let c = setup.to_corners();
        let ex = sub(c.tr, c.tl);
        let ey = sub(c.bl, c.tl);
        let n = cross(ex, ey);
        let dir = sub(hit, eye);
        let t = dot3(sub(c.tl, eye), n) / dot3(dir, n);
        let p = [
            eye[0] + t * dir[0],
            eye[1] + t * dir[1],
            eye[2] + t * dir[2],
        ];
        let rel = sub(p, c.tl);
        [dot3(rel, ex) / dot3(ex, ex), dot3(rel, ey) / dot3(ey, ey)]
    }

    /// Closed loop, vertical: a setup whose `offset_y_mm` is 30 mm too small
    /// (the user's real bug — gaze lands ~3 cm too high) is fully corrected by
    /// one capture-and-drag. Untilted, so the correction is exact.
    #[test]
    fn corrects_a_vertical_offset_error_end_to_end() {
        let truth = base();
        let mut wrong = truth;
        wrong.offset_y_mm = 10.0;

        let eye = [0.0, 420.0, -650.0];
        let looked_at = point_at(&truth, TARGET[0], TARGET[1]);
        let captured = reported(eye, looked_at, &wrong);

        // The symptom: the tracker reports the gaze ABOVE the cross.
        assert!(captured[1] < TARGET[1], "captured={captured:?}");
        assert!(
            ((TARGET[1] - captured[1]) * truth.height_mm - 30.0).abs() < 1e-6,
            "the error should read as 30 mm, got {}",
            (TARGET[1] - captured[1]) * truth.height_mm
        );

        let (nx, ny) = corrected_offsets(captured, TARGET, &wrong);
        assert!((nx - truth.offset_x_mm).abs() < 1e-9, "nx={nx}");
        assert!((ny - truth.offset_y_mm).abs() < 1e-9, "ny={ny}");
    }

    /// Closed loop, horizontal: an `offset_x_mm` error is nulled the same way.
    #[test]
    fn corrects_a_horizontal_offset_error_end_to_end() {
        let mut truth = base();
        truth.offset_x_mm = 45.0;
        let mut wrong = truth;
        wrong.offset_x_mm = -20.0;

        let eye = [10.0, 430.0, -700.0];
        let looked_at = point_at(&truth, TARGET[0], TARGET[1]);
        let captured = reported(eye, looked_at, &wrong);

        // Increasing offset_x moves the reported dot LEFT, so under-reporting
        // offset_x by 65 mm puts the dot to the RIGHT of the cross.
        assert!(captured[0] > TARGET[0], "captured={captured:?}");

        let (nx, ny) = corrected_offsets(captured, TARGET, &wrong);
        assert!((nx - truth.offset_x_mm).abs() < 1e-9, "nx={nx}");
        assert!((ny - truth.offset_y_mm).abs() < 1e-9, "ny={ny}");
    }

    /// Both axes wrong at once, still nulled in a single pass.
    #[test]
    fn corrects_both_axes_at_once() {
        let mut truth = base();
        truth.offset_x_mm = -30.0;
        truth.offset_y_mm = 55.0;
        let mut wrong = truth;
        wrong.offset_x_mm = 25.0;
        wrong.offset_y_mm = 10.0;

        let eye = [-40.0, 400.0, -600.0];
        let looked_at = point_at(&truth, TARGET[0], TARGET[1]);
        let captured = reported(eye, looked_at, &wrong);
        let (nx, ny) = corrected_offsets(captured, TARGET, &wrong);
        assert!((nx - truth.offset_x_mm).abs() < 1e-9, "nx={nx}");
        assert!((ny - truth.offset_y_mm).abs() < 1e-9, "ny={ny}");
    }

    /// On a tilted screen the correction is first-order, not exact: moving the
    /// plane also moves where the gaze ray pierces it. It must still remove the
    /// overwhelming majority of the error in one pass, and converge if repeated.
    #[test]
    fn a_tilted_screen_is_corrected_to_within_a_few_millimetres() {
        let mut truth = base();
        truth.tilt_deg = 20.0;
        truth.offset_y_mm = 60.0;
        let mut wrong = truth;
        wrong.offset_y_mm = 10.0;

        let eye = [0.0, 430.0, -680.0];
        let looked_at = point_at(&truth, TARGET[0], TARGET[1]);

        let captured = reported(eye, looked_at, &wrong);
        let (_, ny) = corrected_offsets(captured, TARGET, &wrong);
        let residual = (ny - truth.offset_y_mm).abs();
        assert!(residual < 5.0, "one pass left {residual} mm");
        assert!(residual < 50.0 * 0.15, "one pass removed <85% of the error");

        // A second pass (capture again with the improved setup) converges.
        let mut better = wrong;
        better.offset_y_mm = ny;
        let captured2 = reported(eye, looked_at, &better);
        let (_, ny2) = corrected_offsets(captured2, TARGET, &better);
        assert!(
            (ny2 - truth.offset_y_mm).abs() < residual,
            "a second pass must improve on the first"
        );
    }

    /// No drag at all must be a no-op, not a nudge.
    #[test]
    fn zero_drag_leaves_the_offsets_untouched() {
        let s = base();
        for p in [[0.5, 0.5], [0.0, 0.0], [1.0, 1.0], [0.13, 0.87]] {
            let (nx, ny) = corrected_offsets(p, p, &s);
            assert_eq!((nx, ny), (s.offset_x_mm, s.offset_y_mm));
        }
    }

    /// Signs, stated independently of the closed loop.
    #[test]
    fn signs_are_right_in_both_axes() {
        let s = base();
        // Dragging the marker RIGHT (target right of captured) means the tracker
        // was reporting too far left, which means offset_x was too large.
        let (nx, _) = corrected_offsets([0.4, 0.5], [0.5, 0.5], &s);
        assert!(nx < s.offset_x_mm, "nx={nx}");
        assert!((nx - (s.offset_x_mm - 0.1 * s.width_mm)).abs() < 1e-9);
        // Dragging the marker DOWN means the screen's bottom edge is higher
        // above the tracker than configured.
        let (_, ny) = corrected_offsets([0.5, 0.4], [0.5, 0.5], &s);
        assert!(ny > s.offset_y_mm, "ny={ny}");
        assert!((ny - (s.offset_y_mm + 0.1 * s.height_mm)).abs() < 1e-9);
    }

    /// Garbage in must not put NaN in the user's config file.
    #[test]
    fn garbage_input_never_yields_nan() {
        let s = base();
        let junk = [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            1e308,
            -1e308,
            0.0,
        ];
        for &a in &junk {
            for &b in &junk {
                let (nx, ny) = corrected_offsets([a, b], [b, a], &s);
                assert!(nx.is_finite() && ny.is_finite(), "a={a} b={b}");
            }
        }
        // A degenerate setup is just as dangerous as a degenerate drag.
        let mut bad = s;
        bad.width_mm = f64::NAN;
        bad.height_mm = f64::INFINITY;
        let (nx, ny) = corrected_offsets([0.1, 0.2], [0.9, 0.8], &bad);
        assert_eq!((nx, ny), (bad.offset_x_mm, bad.offset_y_mm));
    }
}
