//! Fullscreen follow-the-dot calibration flow. The user picks Quick/Full, then
//! follows a pulsing dot; each point is sampled by the device thread (see
//! `device::DeviceCommand::Cal*`). The point sets + `CalMode` are unit-tested;
//! the GTK window + cairo dot (added next) are live-validated.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::{cairo, Align, Application, Button, DrawingArea, Label, Orientation, Overlay};

use crate::device::{next_cal_token, CalPhase, DeviceCommand, DeviceState};
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

// Every UI deadline below must outlast the device-thread work it is waiting on.
// If the UI gives up first the device thread keeps running the old command and
// will not dequeue the abort for the remaining difference — the window in which
// a queued CalBegin/CalFinish can still land on a session the UI has abandoned.
//
// `CalBegin` runs set_enabled_eye + start + clear, three requests each bounded
// by `tobii-usb` DEFAULT_REQUEST_TIMEOUT (10 s).
const START_TIMEOUT_TICKS: u32 = 1000; // ~33 s waiting for the device to ack CalBegin

// One point is bounded by `tobii-usb` CAL_POINT_TIMEOUT (30 s) — keep this
// above it, or a point the USB layer would still have acked is failed here.
const COLLECT_TIMEOUT_TICKS: u32 = 1000; // ~33 s per point before giving up

// Compute+retrieve are two separate device requests, each bounded by the USB
// response deadline (`tobii-usb` DEFAULT_REQUEST_TIMEOUT, 10 s), and the device
// thread may still be finishing an earlier command before either runs. ~45 s
// stays comfortably above that worst case — keep it in step with that deadline.
const COMPUTE_TIMEOUT_TICKS: u32 = 1350; // ~45 s for compute+retrieve

/// UI-side flow state (distinct from the device's `CalPhase`).
#[derive(Clone)]
enum Phase {
    Chooser,
    /// `CalBegin` sent; waiting for the device thread to publish a `CalPhase`
    /// carrying *our* `token`. The token is what makes this an edge and not a
    /// level: `active`, `collected`, `last_error` and `finished` all persist
    /// from the previous session until the device thread dequeues a command,
    /// and it can be blocked in a 30 s USB request while the UI ticks every
    /// 33 ms. Testing those fields directly would let leftovers from the last
    /// run satisfy the gate, walk every point without sampling any, and then
    /// compute + persist a calibration built from zero new points.
    Starting {
        mode: CalMode,
        token: u64,
        ticks: u32,
    },
    Collecting {
        /// The session token this phase belongs to (see `Starting`).
        token: u64,
        mode: CalMode,
        index: usize,
        requested: bool,
        ticks: u32,
    },
    Computing {
        /// The session token this phase belongs to (see `Starting`).
        token: u64,
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
    retry: &Button,
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
        Phase::Starting { .. } => {
            instr.set_text("Starting calibration…");
            chooser.set_visible(false);
            done_box.set_visible(false);
            cancel.set_visible(true);
        }
        Phase::Collecting {
            index,
            mode,
            requested,
            ..
        } => {
            let progress = format!("point {} of {}", index + 1, mode.points().len());
            // While `requested` the device is actually sampling this point.
            // Users saccade away once a target stops moving, which degrades the
            // sample invisibly — so say explicitly when fixation matters.
            instr.set_text(&if *requested {
                format!("Hold still — keep looking at the dot  ·  {progress}")
            } else {
                format!("Follow the dot with your eyes  ·  {progress}")
            });
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
            // Success offers only Done; Retry belongs to the failure screen.
            retry.set_visible(res.is_err());
            cancel.set_visible(false);
        }
    }
}

/// Open the fullscreen follow-the-dot calibration flow, returning the window so
/// the caller can react to it closing (the hub re-enables its button).
pub fn launch(
    app: &Application,
    state: Arc<Mutex<DeviceState>>,
    cmd_tx: Sender<DeviceCommand>,
) -> gtk::ApplicationWindow {
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

    // Chooser -> begin calibration for the chosen mode. The flow waits in
    // `Starting` until the device thread acknowledges the new session; it must
    // not trust any counters until then (see `Phase::Starting`).
    let start_mode: Rc<dyn Fn(CalMode)> = {
        let phase = phase.clone();
        let cmd_tx = cmd_tx.clone();
        Rc::new(move |mode: CalMode| {
            // The chooser buttons stay clickable until the next tick hides
            // them, so a double-click would otherwise send a second CalBegin
            // and issue `start` on an already-open realm. Bind the check to
            // drop the shared borrow before the borrow_mut below.
            let in_chooser = matches!(&*phase.borrow(), Phase::Chooser);
            if !in_chooser {
                return;
            }
            let token = next_cal_token();
            let _ = cmd_tx.send(DeviceCommand::CalBegin { eye, token });
            *phase.borrow_mut() = Phase::Starting {
                mode,
                token,
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
    // Every exit routes through `win.close()` so the single close handler below
    // is the one place that aborts the session and stops the tick.
    {
        let win = win.clone();
        done_btn.connect_clicked(move |_| win.close());
    }
    {
        let win = win.clone();
        cancel.connect_clicked(move |_| win.close());
    }

    // Esc cancels.
    let keys = gtk::EventControllerKey::new();
    {
        let win = win.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                win.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
    }
    win.add_controller(keys);

    // Closing the window — by button, Esc, or the compositor (Alt+F4) — aborts
    // any open session and retires the tick. Without the flag the tick would
    // keep firing forever against a dead window, and could still fire CalFinish
    // and persist a calibration after the flow was gone.
    let closed = Rc::new(Cell::new(false));
    {
        let closed = closed.clone();
        let cmd_tx = cmd_tx.clone();
        win.connect_close_request(move |_| {
            closed.set(true);
            let _ = cmd_tx.send(DeviceCommand::CalAbort);
            glib::Propagation::Proceed
        });
    }

    // ~30 fps state machine: read the device's CalPhase, advance the UI phase.
    let tick_cmd = cmd_tx.clone();
    glib::timeout_add_local(Duration::from_millis(33), move || {
        if closed.get() {
            return glib::ControlFlow::Break;
        }
        let cal: CalPhase = state.lock().unwrap().calibration.clone();
        let mut ph = phase.borrow_mut();
        let mut next: Option<Phase> = None;
        match &*ph {
            Phase::Chooser => {
                dot.borrow_mut().point = None;
            }
            Phase::Starting { mode, token, ticks } => {
                let (mode, token, ticks) = (*mode, *token, *ticks);
                dot.borrow_mut().point = None;
                // Keep waiting while EITHER our CalBegin has not been dequeued
                // (token mismatch — nothing in `cal` is ours, so not `finished`,
                // not `last_error`, not `active`, not `collected` may be read)
                // OR it has been dequeued but `start`/`clear` are still in
                // flight. `started` — not `active` — is the "session is really
                // open" signal: `active` is set before any USB traffic, so
                // gating on it would enter Collecting against an unopened realm
                // and queue a stray point request behind a start that may fail.
                if cal.token != token || (!cal.started && cal.finished.is_none()) {
                    let t = ticks + 1;
                    if t >= START_TIMEOUT_TICKS {
                        // The abort is queued *behind* our CalBegin, so the
                        // device thread still closes whatever CalBegin opened.
                        let _ = tick_cmd.send(DeviceCommand::CalAbort);
                        next = Some(Phase::Done(Err(
                            "Could not start calibration. Check that the eye tracker is connected."
                                .into(),
                        )));
                    } else {
                        next = Some(Phase::Starting {
                            mode,
                            token,
                            ticks: t,
                        });
                    }
                } else if cal.started {
                    // start + clear are both acked: the session is really open
                    // and the counters below are ours, starting from zero.
                    next = Some(Phase::Collecting {
                        token,
                        mode,
                        index: 0,
                        requested: false,
                        ticks: 0,
                    });
                } else {
                    // Our CalBegin ran and its start/clear failed. `start` may
                    // still have succeeded (only `clear` failing), leaving the
                    // device in an open session — so abort explicitly.
                    let msg = match &cal.finished {
                        Some(Err(e)) => e.clone(),
                        _ => "Could not start calibration.".to_string(),
                    };
                    let _ = tick_cmd.send(DeviceCommand::CalAbort);
                    next = Some(Phase::Done(Err(msg)));
                }
            }
            Phase::Collecting {
                token,
                mode,
                index,
                requested,
                ticks,
            } => {
                let (token, mode, index, requested, ticks) =
                    (*token, *mode, *index, *requested, *ticks);
                if cal.token != token {
                    // Another session replaced ours — only reachable if a second
                    // flow window ever opened. Never act on counters that are not
                    // ours; that is precisely what the token exists to prevent.
                    next = Some(Phase::Done(Err("Calibration was interrupted.".into())));
                } else if let Some(Err(e)) = &cal.finished {
                    // Defensive: within a session nothing finishes it but our
                    // own CalFinish (which leaves for `Computing`). If a finish
                    // does surface here the session may still be open, so stop
                    // it explicitly.
                    let _ = tick_cmd.send(DeviceCommand::CalAbort);
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
                        next = Some(Phase::Computing { token, ticks: 0 });
                    } else {
                        next = Some(Phase::Collecting {
                            token,
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
                            token,
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
                            token,
                            mode,
                            index,
                            requested,
                            ticks: t,
                        });
                    }
                }
            }
            Phase::Computing { token, ticks } => {
                let token = *token;
                dot.borrow_mut().point = None;
                if cal.token != token {
                    // Another session replaced ours; `finished` below would be
                    // someone else's outcome, so never report it as our own.
                    next = Some(Phase::Done(Err("Calibration was interrupted.".into())));
                } else if let Some(res) = &cal.finished {
                    next = Some(Phase::Done(res.clone()));
                } else {
                    let t = ticks + 1;
                    if t >= COMPUTE_TIMEOUT_TICKS {
                        let _ = tick_cmd.send(DeviceCommand::CalAbort);
                        next = Some(Phase::Done(
                            Err("Calibration computation timed out.".into()),
                        ));
                    } else {
                        next = Some(Phase::Computing { token, ticks: t });
                    }
                }
            }
            Phase::Done(_) => {}
        }
        if let Some(n) = next {
            *ph = n;
        }
        update_ui(&ph, &instr, &chooser, &done_box, &retry_btn, &cancel);
        area.queue_draw();
        glib::ControlFlow::Continue
    });

    win.present();
    win
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
