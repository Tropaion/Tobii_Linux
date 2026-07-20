//! Fullscreen follow-the-dot calibration flow. The user picks Quick/Full, then
//! follows a pulsing dot; each point is sampled by the device thread (see
//! `device::DeviceCommand::Cal*`). The point sets + `CalMode` are unit-tested;
//! the GTK window + cairo dot (added next) are live-validated.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::{cairo, Align, Application, Button, DrawingArea, Label, Orientation, Overlay};

use crate::device::{CalPhase, DeviceCommand, DeviceState};
use tobii_protocol::EnabledEye;

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

// Tick cadence is 33 ms (~30 fps), matching the hub.
const SETTLE_TICKS: u32 = 8; // ~260 ms saccade settle before sampling a point
const COLLECT_TIMEOUT_TICKS: u32 = 300; // ~10 s per point before giving up
const COMPUTE_TIMEOUT_TICKS: u32 = 450; // ~15 s for compute+retrieve

/// UI-side flow state (distinct from the device's `CalPhase`).
#[derive(Clone)]
enum Phase {
    Chooser,
    Collecting {
        mode: CalMode,
        index: usize,
        requested: bool,
        ticks: u32,
    },
    Computing {
        ticks: u32,
    },
    Done(Result<(), String>),
}

/// What the cairo surface draws this frame.
struct DotView {
    point: Option<(f64, f64)>,
    pulse: f64,
}

/// Primary monitor height (px), to place the header a bit above the middle.
fn screen_height() -> i32 {
    gtk::gdk::Display::default()
        .and_then(|d| d.monitors().item(0))
        .and_then(|o| o.downcast::<gtk::gdk::Monitor>().ok())
        .map(|m| m.geometry().height())
        .filter(|h| *h > 0)
        .unwrap_or(1080)
}

/// Dark background + the pulsing fixation dot (a ring converging on a dot).
fn draw_scene(cr: &cairo::Context, w: i32, h: i32, dot: &DotView) {
    let (w, h) = (w as f64, h as f64);
    cr.set_source_rgb(0.08, 0.09, 0.11);
    let _ = cr.paint();
    if let Some((nx, ny)) = dot.point {
        let (cx, cy) = (nx * w, ny * h);
        let ring = 26.0 + (1.0 - dot.pulse) * 22.0;
        cr.set_source_rgba(0.30, 0.85, 0.85, 0.6 * dot.pulse.max(0.15));
        cr.set_line_width(3.0);
        cr.arc(cx, cy, ring, 0.0, std::f64::consts::TAU);
        let _ = cr.stroke();
        cr.set_source_rgb(0.30, 0.85, 0.85);
        cr.arc(cx, cy, 7.0, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
    }
}

/// Reflect the current phase in the instruction text + visible controls.
fn update_ui(
    phase: &Phase,
    instr: &Label,
    chooser: &gtk::Box,
    done_box: &gtk::Box,
    cancel: &Button,
) {
    match phase {
        Phase::Chooser => {
            instr.set_text(
                "Choose a calibration. Sit comfortably, about an arm's length from the screen.",
            );
            chooser.set_visible(true);
            done_box.set_visible(false);
            cancel.set_visible(true);
        }
        Phase::Collecting { index, mode, .. } => {
            instr.set_text(&format!(
                "Follow the dot with your eyes  ·  point {} of {}",
                index + 1,
                mode.points().len()
            ));
            chooser.set_visible(false);
            done_box.set_visible(false);
            cancel.set_visible(true);
        }
        Phase::Computing { .. } => {
            instr.set_text("Computing your calibration…");
            chooser.set_visible(false);
            done_box.set_visible(false);
            cancel.set_visible(false);
        }
        Phase::Done(res) => {
            match res {
                Ok(()) => instr.set_text("Calibration complete."),
                Err(e) => instr.set_text(e),
            }
            chooser.set_visible(false);
            done_box.set_visible(true);
            cancel.set_visible(false);
        }
    }
}

/// Open the fullscreen follow-the-dot calibration flow.
pub fn launch(app: &Application, state: Arc<Mutex<DeviceState>>, cmd_tx: Sender<DeviceCommand>) {
    // Eye to calibrate: the device's current selection, defaulting to Both.
    let eye = state
        .lock()
        .unwrap()
        .enabled_eye
        .unwrap_or(EnabledEye::Both);

    let win = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Calibration")
        .build();
    win.set_modal(true);
    win.fullscreen();

    let phase = Rc::new(RefCell::new(Phase::Chooser));
    let dot = Rc::new(RefCell::new(DotView {
        point: None,
        pulse: 0.0,
    }));

    let area = DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    {
        let dot = dot.clone();
        area.set_draw_func(move |_, cr, w, h| draw_scene(cr, w, h, &dot.borrow()));
    }

    let instr = Label::new(Some("Calibrate your eye tracker"));
    instr.add_css_class("app-title");
    instr.set_halign(Align::Center);
    instr.set_justify(gtk::Justification::Center);
    instr.set_wrap(true);

    let quick = Button::with_label("Quick (5 points)");
    let full = Button::with_label("Full (9 points)");
    let chooser = gtk::Box::new(Orientation::Horizontal, 10);
    chooser.set_halign(Align::Center);
    chooser.append(&quick);
    chooser.append(&full);

    let done_btn = Button::with_label("Done");
    let retry_btn = Button::with_label("Retry");
    let done_box = gtk::Box::new(Orientation::Horizontal, 10);
    done_box.set_halign(Align::Center);
    done_box.append(&retry_btn);
    done_box.append(&done_btn);
    done_box.set_visible(false);

    let cancel = Button::with_label("Cancel");
    cancel.set_halign(Align::Center);

    let header = gtk::Box::new(Orientation::Vertical, 16);
    header.set_halign(Align::Center);
    header.set_valign(Align::Start);
    header.set_margin_top((screen_height() as f64 * 0.30) as i32);
    header.append(&instr);
    header.append(&chooser);
    header.append(&done_box);
    header.append(&cancel);

    let overlay = Overlay::new();
    overlay.set_child(Some(&area));
    overlay.add_overlay(&header);
    win.set_child(Some(&overlay));

    // Chooser -> begin calibration for the chosen mode.
    let start_mode: Rc<dyn Fn(CalMode)> = {
        let phase = phase.clone();
        let cmd_tx = cmd_tx.clone();
        Rc::new(move |mode: CalMode| {
            let _ = cmd_tx.send(DeviceCommand::CalBegin { eye });
            *phase.borrow_mut() = Phase::Collecting {
                mode,
                index: 0,
                requested: false,
                ticks: 0,
            };
        })
    };
    {
        let s = start_mode.clone();
        quick.connect_clicked(move |_| s(CalMode::Quick));
    }
    {
        let s = start_mode.clone();
        full.connect_clicked(move |_| s(CalMode::Full));
    }
    {
        let phase = phase.clone();
        retry_btn.connect_clicked(move |_| *phase.borrow_mut() = Phase::Chooser);
    }
    {
        let win = win.clone();
        let cmd_tx = cmd_tx.clone();
        done_btn.connect_clicked(move |_| {
            let _ = cmd_tx.send(DeviceCommand::CalAbort);
            win.close();
        });
    }
    {
        let win = win.clone();
        let cmd_tx = cmd_tx.clone();
        cancel.connect_clicked(move |_| {
            let _ = cmd_tx.send(DeviceCommand::CalAbort);
            win.close();
        });
    }

    // Esc cancels.
    let keys = gtk::EventControllerKey::new();
    {
        let win = win.clone();
        let cmd_tx = cmd_tx.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                let _ = cmd_tx.send(DeviceCommand::CalAbort);
                win.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
    }
    win.add_controller(keys);

    // ~30 fps state machine: read the device's CalPhase, advance the UI phase.
    let tick_cmd = cmd_tx.clone();
    glib::timeout_add_local(Duration::from_millis(33), move || {
        let cal: CalPhase = state.lock().unwrap().calibration.clone();
        let mut ph = phase.borrow_mut();
        let mut next: Option<Phase> = None;
        match &*ph {
            Phase::Chooser => {
                dot.borrow_mut().point = None;
            }
            Phase::Collecting {
                mode,
                index,
                requested,
                ticks,
            } => {
                let (mode, index, requested, ticks) = (*mode, *index, *requested, *ticks);
                if let Some(Err(e)) = &cal.finished {
                    next = Some(Phase::Done(Err(e.clone())));
                } else if let Some(e) = &cal.last_error {
                    let _ = tick_cmd.send(DeviceCommand::CalAbort);
                    next = Some(Phase::Done(Err(format!(
                        "Couldn't read a point: {e}. Make sure you're seated and looking at the dots."
                    ))));
                } else if cal.collected > index {
                    let pts = mode.points();
                    if index + 1 >= pts.len() {
                        let _ = tick_cmd.send(DeviceCommand::CalFinish);
                        next = Some(Phase::Computing { ticks: 0 });
                    } else {
                        next = Some(Phase::Collecting {
                            mode,
                            index: index + 1,
                            requested: false,
                            ticks: 0,
                        });
                    }
                } else {
                    let (px, py) = mode.points()[index];
                    {
                        let mut d = dot.borrow_mut();
                        d.point = Some((px, py));
                        d.pulse = (d.pulse + 0.06) % 1.0;
                    }
                    let t = ticks + 1;
                    if !requested && t >= SETTLE_TICKS {
                        let _ = tick_cmd.send(DeviceCommand::CalCollect { x: px, y: py });
                        next = Some(Phase::Collecting {
                            mode,
                            index,
                            requested: true,
                            ticks: 0,
                        });
                    } else if requested && t >= COLLECT_TIMEOUT_TICKS {
                        let _ = tick_cmd.send(DeviceCommand::CalAbort);
                        next = Some(Phase::Done(Err("Timed out reading a point.".into())));
                    } else {
                        next = Some(Phase::Collecting {
                            mode,
                            index,
                            requested,
                            ticks: t,
                        });
                    }
                }
            }
            Phase::Computing { ticks } => {
                dot.borrow_mut().point = None;
                if let Some(res) = &cal.finished {
                    next = Some(Phase::Done(res.clone()));
                } else {
                    let t = ticks + 1;
                    if t >= COMPUTE_TIMEOUT_TICKS {
                        next = Some(Phase::Done(
                            Err("Calibration computation timed out.".into()),
                        ));
                    } else {
                        next = Some(Phase::Computing { ticks: t });
                    }
                }
            }
            Phase::Done(_) => {}
        }
        if let Some(n) = next {
            *ph = n;
        }
        update_ui(&ph, &instr, &chooser, &done_box, &cancel);
        area.queue_draw();
        glib::ControlFlow::Continue
    });

    win.present();
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
