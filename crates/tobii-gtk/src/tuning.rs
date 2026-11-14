//! The "Tuning" window: gaze, smoothing and the per-axis response curves,
//! editable while the head is being tracked.
//!
//! Everything here is a `games.toml` key that `tobii games set` can also reach;
//! this window is the version of those keys a person can tune by feel.
//!
//! # Writing, and reading back
//!
//! A control's change is queued, and the queue is written through
//! [`edit_config`] — which re-reads the file first, so this window, the Games
//! row and a terminal never overwrite each other's changes — [`SAVE_AFTER`]
//! after the first change, when the window loses focus and when it closes. A
//! slider passes through every step on its way, and each write is a read, an
//! fsync and a rename on the GTK thread: written per step, one drag was dozens
//! of them a second. The hub re-reads the file once a second, so a change is
//! felt in game a moment after it is made.
//!
//! The controls re-read the file whenever it changes (a [`gio::FileMonitor`]),
//! so a change made on the Games row or from a terminal shows here.
//!
//! [`gio::FileMonitor`]: gtk::gio::FileMonitor
//!
//! # The curve editor
//!
//! One plot per axis, positive half only — every curve is odd-symmetric, so the
//! negative half is the same shape mirrored. Three handles:
//!
//! * the **end point** — drag it sideways for how far the head must move to
//!   reach full output (`input_max`) and up and down for what full output is
//!   (`output_max`);
//! * the **deadzone** on the floor — how far the head may move before anything
//!   happens;
//! * the **bend** at the middle of the span — drag it down for a gentle centre
//!   and a steep edge, up for the opposite. It sets a `power:N` curve.
//!
//! A live marker shows where the head is on the curve right now, so the effect
//! of a change is visible without a game open.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{cairo, glib, Adjustment, Align, Label, Orientation};

use tobii_headpose::HeadPose;
use tobii_output::curves::DEFAULT_INVERT;
use tobii_output::fusion::{AxisResponse, Curve};
use tobii_output::games::{load_output_config, FilterMode, OutputConfig};

use crate::games::edit_config;

/// Where the live pose comes from: the hub's device state.
pub type Live = Rc<dyn Fn() -> Option<HeadPose>>;

/// How long changes wait to be written after the first of them, so that a
/// slider drag is a write every so often rather than one per step.
pub const SAVE_AFTER: Duration = Duration::from_millis(300);

/// The window's title, which is also how a test finds it.
pub const TITLE: &str = "Head tracking tuning";

thread_local! {
    /// The open window, so a second click raises it rather than opening a twin
    /// that would fight the first over the same keys.
    static OPEN: RefCell<Option<glib::WeakRef<gtk::Window>>> = const { RefCell::new(None) };
}

/// One slider's name and tooltip.
///
/// The help window's *Tuning window* topic quotes every one of these: GTK4
/// shows a tooltip to a pointer and to nothing else, so a keyboard or touch
/// user reads them there (see [`crate::help`]).
pub(crate) struct Tip {
    pub name: &'static str,
    pub text: &'static str,
}

const GAZE_YAW: Tip = Tip {
    name: "Sideways reach",
    text: "How far the view turns when you look at the left or right edge. Past \
           `ev_yaw_clamp_deg` it turns no further, it only gets there sooner",
};
const GAZE_PITCH: Tip = Tip {
    name: "Vertical reach",
    text: "How far the view tilts when you look at the top or bottom edge. Past \
           `ev_pitch_clamp_deg` it tilts no further, it only gets there sooner",
};
const CUTOFF: Tip = Tip {
    name: "Steadiness at rest",
    text: "Cutoff while the head is still. Lower = steadier, but slower to settle",
};
const BETA: Tip = Tip {
    name: "Speed response",
    text: "How quickly the filter opens up as you move. Higher = less lag on fast turns, \
           but noise can open it too",
};
const ALPHA: Tip = Tip {
    name: "Follow strength",
    text: "Weight of each new sample. Lower = smoother and laggier; 1 = no smoothing",
};
const ROT: Tip = Tip {
    name: "Max turn speed",
    text: "The fastest the output may rotate. Lower turns jumps into sweeps; too low feels \
           sluggish",
};
const MOV: Tip = Tip {
    name: "Max move speed",
    text: "The fastest the output may translate",
};
const JUMP: Tip = Tip {
    name: "Glitch gate",
    text: "A position step larger than this in one frame is treated as a tracking glitch \
           and held",
};
const T_ROT: Tip = Tip {
    name: "Rotation pace",
    text: "How long rotation takes to drift back to centre. Lower = stronger pull",
};
const T_POS: Tip = Tip {
    name: "Position pace",
    text: "The same for leaning. Lower = stronger pull back to centre",
};
const WINDOW: Tip = Tip {
    name: "Ignore turns past",
    text: "A turn bigger than this is deliberate and is not pulled back",
};
const DEADZONE: Tip = Tip {
    name: "Deadzone",
    text: "Movement from centre that does nothing at all",
};
const INPUT: Tip = Tip {
    name: "Head movement for full output",
    text: "How far you move your head to reach the full output below",
};
const OUTPUT: Tip = Tip {
    name: "Full output",
    text: "What the game receives at that head movement",
};
const EXPONENT: Tip = Tip {
    name: "Power exponent",
    text: "Above 1: gentle near centre, steep at the edge. Below 1: the opposite",
};

/// Every slider, in the order the window shows them.
pub(crate) const TIPS: [Tip; 15] = [
    DEADZONE, INPUT, OUTPUT, EXPONENT, CUTOFF, BETA, ALPHA, ROT, MOV, JUMP, T_ROT, T_POS, WINDOW,
    GAZE_YAW, GAZE_PITCH,
];

/// What the editor knows about one axis besides its curve.
struct AxisInfo {
    title: &'static str,
    unit: &'static str,
    /// Slider ranges: deadzone, input max, output max.
    deadzone_max: f64,
    input_max: f64,
    output_max: f64,
    /// The plot's smallest x extent: roughly how far a head can move on this
    /// axis while the tracker still sees it.
    plot_x: f64,
}

static AXES: [AxisInfo; 6] = [
    AxisInfo {
        title: "Yaw",
        unit: "°",
        deadzone_max: 20.0,
        input_max: 180.0,
        output_max: 180.0,
        plot_x: 45.0,
    },
    AxisInfo {
        title: "Pitch",
        unit: "°",
        deadzone_max: 20.0,
        input_max: 90.0,
        output_max: 180.0,
        plot_x: 30.0,
    },
    AxisInfo {
        title: "Roll",
        unit: "°",
        deadzone_max: 20.0,
        input_max: 180.0,
        output_max: 180.0,
        plot_x: 30.0,
    },
    AxisInfo {
        title: "X (sideways)",
        unit: "mm",
        deadzone_max: 100.0,
        input_max: 500.0,
        output_max: 1000.0,
        plot_x: 150.0,
    },
    AxisInfo {
        title: "Y (up/down)",
        unit: "mm",
        deadzone_max: 100.0,
        input_max: 500.0,
        output_max: 1000.0,
        plot_x: 120.0,
    },
    AxisInfo {
        title: "Z (forward/back)",
        unit: "mm",
        deadzone_max: 100.0,
        input_max: 500.0,
        output_max: 1000.0,
        plot_x: 150.0,
    },
];

/// The value of axis `i` in `p`, in the order of [`AXIS_NAMES`].
fn axis_value(p: &HeadPose, i: usize) -> f64 {
    [p.yaw_deg, p.pitch_deg, p.roll_deg, p.x_mm, p.y_mm, p.z_mm][i]
}

/// A change to the config: a queued edit, or a control's re-read.
type ConfigFn = Box<dyn Fn(&mut OutputConfig)>;

/// What the window's controls share: the edits waiting to be written, and how
/// to put the file's values back into the controls.
///
/// Owned by the window's own handlers and reached weakly from every control's.
/// The reloads hold the controls, so a control's handler holding this would be
/// a cycle (see the crate docs, *Lifetimes*).
#[derive(Default)]
struct Ctx {
    /// Edits waiting to be written, applied in order. A write is due
    /// [`SAVE_AFTER`] after the first.
    edits: RefCell<Vec<ConfigFn>>,
    /// Set while the file's values are being put into the controls, so their
    /// handlers do not queue back what was just read.
    loading: Cell<bool>,
    /// Put the file's values into one section's controls each.
    reloads: RefCell<Vec<ConfigFn>>,
}

impl Ctx {
    /// Queue `edit`, to be written within [`SAVE_AFTER`].
    fn queue(self: &Rc<Self>, edit: impl Fn(&mut OutputConfig) + 'static) {
        if self.loading.get() {
            return;
        }
        let mut edits = self.edits.borrow_mut();
        edits.push(Box::new(edit));
        if edits.len() > 1 {
            return;
        }
        // The first of a batch. A timer left over from a batch written early
        // (on focus out) finds the queue empty, or writes the next batch a
        // little sooner, either of which is harmless.
        let ctx = Rc::downgrade(self);
        glib::timeout_add_local_once(SAVE_AFTER, move || {
            if let Some(ctx) = ctx.upgrade() {
                ctx.write();
            }
        });
    }

    /// Write what is queued, now.
    fn write(&self) {
        let edits = self.edits.take();
        if !edits.is_empty() {
            edit_config("tuning", |c| edits.iter().for_each(|e| e(c)));
        }
    }

    /// Write what is queued, then put the file's values into every control.
    fn sync(&self) {
        self.write();
        self.reload();
    }

    /// Put the file's values into every control.
    fn reload(&self) {
        let mut cfg = load_output_config();
        self.loading.set(true);
        for reload in self.reloads.borrow().iter() {
            reload(&mut cfg);
        }
        self.loading.set(false);
    }

    /// Run `reload` with the file's values whenever the window re-reads it.
    fn on_reload(&self, reload: impl Fn(&mut OutputConfig) + 'static) {
        self.reloads.borrow_mut().push(Box::new(reload));
    }

    /// An adjustment over the number `field` reaches: each change is queued,
    /// and a re-read puts the file's value back — widened to fit, so a value
    /// set from a terminal shows as it is rather than as the nearest end.
    fn bound(
        self: &Rc<Self>,
        field: fn(&mut OutputConfig) -> &mut f64,
        range: (f64, f64),
        step: f64,
    ) -> Adjustment {
        let adj = Adjustment::new(range.0, range.0, range.1, step, step * 10.0, 0.0);
        let ctx = Rc::downgrade(self);
        adj.connect_value_changed(move |a| {
            let v = a.value();
            if let Some(ctx) = ctx.upgrade() {
                ctx.queue(move |c| *field(c) = v);
            }
        });
        let a = adj.clone();
        self.on_reload(move |c| show_widened(&a, range, *field(c)));
        adj
    }

    /// A check button over the flag `field` reaches, with `then` run on every
    /// change, a re-read's included.
    fn toggle(
        self: &Rc<Self>,
        label: &str,
        field: fn(&mut OutputConfig) -> &mut bool,
        then: impl Fn(bool) + 'static,
    ) -> gtk::CheckButton {
        let b = gtk::CheckButton::with_label(label);
        // Unchecked, which the first re-read changes or leaves; a check button
        // says nothing when it is set to what it already is.
        then(false);
        let ctx = Rc::downgrade(self);
        b.connect_toggled(move |b| {
            let v = b.is_active();
            then(v);
            if let Some(ctx) = ctx.upgrade() {
                ctx.queue(move |c| *field(c) = v);
            }
        });
        let w = b.clone();
        self.on_reload(move |c| w.set_active(*field(c)));
        b
    }
}

impl Drop for Ctx {
    /// Whatever is queued still goes to the file. The close writes first, but
    /// GTK clears the window's focus later, while tearing it down, and a spin
    /// button commits a typed value when it loses focus — after that write.
    fn drop(&mut self) {
        self.write();
    }
}

/// Show `v` on `adj` over `range`, widened to fit `v` rather than clamping it.
///
/// A value set from a terminal can lie outside a slider's range. Clamped, the
/// slider showed a number not in effect, and the curve editor — whose every
/// change writes the whole axis from all of its controls — wrote it back
/// changed by the next edit of anything beside it: `power:8` became 4.
fn show_widened(adj: &Adjustment, (lower, upper): (f64, f64), v: f64) {
    adj.set_lower(lower.min(v));
    adj.set_upper(upper.max(v));
    adj.set_value(v);
}

/// The tracker claim the open window holds: its live dot is a pose, and the
/// hub releases its own claim the moment this window takes the focus.
pub const CLAIM: &str = "the tuning window";

/// Open the window, or raise it if it is already open.
pub fn open(anchor: &impl IsA<gtk::Widget>, live: Live, demand: &crate::device::Demand) {
    if let Some(w) = OPEN.with(|o| o.borrow().as_ref().and_then(|w| w.upgrade())) {
        w.present();
        return;
    }
    let win = gtk::Window::new();
    win.set_title(Some(TITLE));
    win.set_default_size(820, 760);
    crate::dialog_of(&win, anchor.root().and_downcast::<gtk::Window>().as_ref());
    crate::hold_while_open(demand, &win, CLAIM);

    let ctx = Rc::new(Ctx::default());
    let body = gtk::Box::new(Orientation::Vertical, 12);
    body.set_margin_top(16);
    body.set_margin_bottom(16);
    body.set_margin_start(16);
    body.set_margin_end(16);
    body.append(&curves_section(&ctx, live));
    body.append(&smoothing_section(&ctx));
    body.append(&gaze_section(&ctx));
    // Every control is built neutral and filled from the file here, by the
    // same re-read that later picks up a change made elsewhere.
    ctx.sync();

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroll.set_child(Some(&body));
    win.set_child(Some(&scroll));

    OPEN.with(|o| *o.borrow_mut() = Some(win.downgrade()));
    // The file, watched: a change made on the Games row or from a terminal
    // shows here at once, and a control edited later starts from it, focused
    // or not — a scroll wheel moves a slider in a window that is not. Not
    // while edits are queued, which would put a slider being dragged back to
    // the value it was last written at; the write that empties the queue is
    // itself a change, and re-reads.
    let watch = {
        let ctx = Rc::downgrade(&ctx);
        crate::watch_file(&tobii_output::games::games_path(), move || {
            if let Some(ctx) = ctx.upgrade().filter(|c| c.edits.borrow().is_empty()) {
                ctx.reload();
            }
        })
    };
    // Focus out writes what is queued, so a change is in the file by the time
    // the game has the focus. Focus in re-reads, with nothing queued — what
    // the watch above does already, unless GIO could not watch the file.
    {
        let ctx = ctx.clone();
        win.connect_is_active_notify(move |w| {
            if !w.is_active() {
                ctx.write();
            } else if ctx.edits.borrow().is_empty() {
                ctx.reload();
            }
        });
    }
    win.connect_close_request(move |_| {
        ctx.write();
        let _ = &watch;
        OPEN.with(|o| *o.borrow_mut() = None);
        glib::Propagation::Proceed
    });
    win.present();
}

/// A titled card, styled like the hub's own.
fn card(title: &str, desc: &str) -> gtk::Box {
    let b = gtk::Box::new(Orientation::Vertical, 8);
    b.add_css_class("surface");
    b.add_css_class("panel-pad");
    let t = Label::new(Some(title));
    t.add_css_class("section-title");
    t.set_halign(Align::Start);
    b.append(&t);
    if !desc.is_empty() {
        let d = Label::new(Some(desc));
        d.add_css_class("section-desc");
        d.set_halign(Align::Start);
        d.set_xalign(0.0);
        d.set_wrap(true);
        b.append(&d);
    }
    b
}

/// A labelled slider with a spin button on the same adjustment. Returns the
/// label showing `unit`.
fn slider(
    grid: &gtk::Grid,
    row: i32,
    tip: &Tip,
    adj: &Adjustment,
    digits: u32,
    unit: &str,
) -> Label {
    let l = Label::new(Some(tip.name));
    l.set_halign(Align::Start);
    l.set_xalign(0.0);
    // One width for every slider label, so the sliders line up across the
    // separate grids of each card.
    l.set_width_chars(26);
    let scale = gtk::Scale::new(Orientation::Horizontal, Some(adj));
    scale.set_draw_value(false);
    scale.set_hexpand(true);
    let spin = gtk::SpinButton::new(Some(adj), adj.step_increment(), digits);
    spin.set_width_chars(7);
    let u = Label::new(Some(unit));
    u.set_halign(Align::Start);
    u.set_width_chars(5);
    u.add_css_class("section-desc");
    for w in [
        l.upcast_ref::<gtk::Widget>(),
        scale.upcast_ref(),
        spin.upcast_ref(),
    ] {
        w.set_tooltip_text(Some(tip.text));
    }
    grid.attach(&l, 0, row, 1, 1);
    grid.attach(&scale, 1, row, 1, 1);
    grid.attach(&spin, 2, row, 1, 1);
    grid.attach(&u, 3, row, 1, 1);
    u
}

fn grid() -> gtk::Grid {
    let g = gtk::Grid::new();
    g.set_column_spacing(10);
    g.set_row_spacing(4);
    g
}

// --- Gaze ------------------------------------------------------------------

fn gaze_section(ctx: &Rc<Ctx>) -> gtk::Box {
    let b = card(
        "Gaze (Extended View)",
        "Turns the camera further when you look towards the edge of the screen. \
         Off, the view follows your head alone — steadier, since eye movements are \
         fast and jumpy.",
    );
    let g = grid();
    let on = {
        let g = g.clone();
        ctx.toggle(
            "Gaze steers the camera",
            |c| &mut c.extended_view.enabled,
            move |v| g.set_sensitive(v),
        )
    };
    b.append(&on);
    // Past the ceiling (`clamp_deg`) a reach still matters: it reaches the
    // ceiling sooner, which is what the hub's Strong preset does.
    let yaw = ctx.bound(
        |c| &mut c.extended_view.yaw.output_max_deg,
        (0.0, 90.0),
        1.0,
    );
    let pitch = ctx.bound(
        |c| &mut c.extended_view.pitch.output_max_deg,
        (0.0, 60.0),
        1.0,
    );
    slider(&g, 0, &GAZE_YAW, &yaw, 0, "°");
    slider(&g, 1, &GAZE_PITCH, &pitch, 0, "°");
    b.append(&g);
    b
}

// --- Smoothing -------------------------------------------------------------

fn smoothing_section(ctx: &Rc<Ctx>) -> gtk::Box {
    let b = card(
        "Smoothing",
        "Adaptive smooths hard while your head is still and lets a deliberate turn \
         through quickly. Fixed smooths everything by the same amount. The speed \
         limits stop sudden jumps from reaching the game.",
    );

    let mode_row = gtk::Box::new(Orientation::Horizontal, 10);
    mode_row.append(&Label::new(Some("Filter")));
    // In the drop-down's order.
    const MODES: [FilterMode; 2] = [FilterMode::Adaptive, FilterMode::Ema];
    let index = |m: FilterMode| MODES.iter().position(|x| *x == m).unwrap_or(0) as u32;
    let mode = gtk::DropDown::from_strings(&["Adaptive (One-Euro)", "Fixed (EMA)"]);
    mode_row.append(&mode);
    b.append(&mode_row);

    let adaptive = grid();
    let cutoff = ctx.bound(|c| &mut c.filter_min_cutoff_hz, (0.05, 3.0), 0.05);
    let beta = ctx.bound(|c| &mut c.filter_beta, (0.0, 0.2), 0.001);
    slider(&adaptive, 0, &CUTOFF, &cutoff, 2, "Hz");
    slider(&adaptive, 1, &BETA, &beta, 3, "");
    b.append(&adaptive);

    let ema = grid();
    let alpha = ctx.bound(|c| &mut c.filter_alpha, (0.02, 1.0), 0.01);
    slider(&ema, 0, &ALPHA, &alpha, 2, "");
    b.append(&ema);

    let sel = move |m: u32| {
        adaptive.set_visible(m == 0);
        ema.set_visible(m == 1);
    };
    sel(mode.selected());
    {
        let weak = Rc::downgrade(ctx);
        mode.connect_selected_notify(move |d| {
            let m = d.selected();
            sel(m);
            if let Some(ctx) = weak.upgrade() {
                ctx.queue(move |c| c.filter_mode = MODES[m as usize]);
            }
        });
        let mode = mode.clone();
        ctx.on_reload(move |c| mode.set_selected(index(c.filter_mode)));
    }

    let limits = grid();
    let rot = ctx.bound(|c| &mut c.filter_max_rot_speed_deg_s, (20.0, 2000.0), 10.0);
    let mov = ctx.bound(|c| &mut c.filter_max_move_speed_mm_s, (20.0, 3000.0), 10.0);
    let jump = ctx.bound(|c| &mut c.filter_max_step_mm, (20.0, 1000.0), 5.0);
    slider(&limits, 0, &ROT, &rot, 0, "°/s");
    slider(&limits, 1, &MOV, &mov, 0, "mm/s");
    slider(&limits, 2, &JUMP, &jump, 0, "mm");
    b.append(&limits);

    let acg = grid();
    let ac = {
        let acg = acg.clone();
        ctx.toggle(
            "Auto-centre",
            |c| &mut c.auto_centre.enabled,
            move |v| acg.set_sensitive(v),
        )
    };
    ac.set_tooltip_text(Some(crate::games::AUTO_CENTRE_TOOLTIP));
    b.append(&ac);
    let t_rot = ctx.bound(|c| &mut c.auto_centre.time_s, (0.5, 60.0), 0.5);
    let t_pos = ctx.bound(|c| &mut c.auto_centre.translation_time_s, (0.5, 60.0), 0.5);
    let win = ctx.bound(|c| &mut c.auto_centre.window_deg, (2.0, 45.0), 1.0);
    slider(&acg, 0, &T_ROT, &t_rot, 1, "s");
    slider(&acg, 1, &T_POS, &t_pos, 1, "s");
    slider(&acg, 2, &WINDOW, &win, 0, "°");
    b.append(&acg);

    let reset = crate::widget::button("Reset smoothing to defaults");
    reset.set_halign(Align::Start);
    {
        let d = OutputConfig::default();
        let set: Vec<(Adjustment, f64)> = vec![
            (cutoff, d.filter_min_cutoff_hz),
            (beta, d.filter_beta),
            (alpha, d.filter_alpha),
            (rot, d.filter_max_rot_speed_deg_s),
            (mov, d.filter_max_move_speed_mm_s),
            (jump, d.filter_max_step_mm),
            (t_rot, d.auto_centre.time_s),
            (t_pos, d.auto_centre.translation_time_s),
            (win, d.auto_centre.window_deg),
        ];
        // The drop-down weakly: it carries a handler of its own.
        let mode = mode.downgrade();
        reset.connect_clicked(move |_| {
            if let Some(mode) = mode.upgrade() {
                mode.set_selected(index(d.filter_mode));
            }
            for (a, v) in &set {
                a.set_value(*v);
            }
        });
    }
    b.append(&reset);
    b
}

// --- Curves ----------------------------------------------------------------

/// The editor's state, shared by its controls, the plot and the drag.
struct Editor {
    axis: Cell<usize>,
    resp: Cell<AxisResponse>,
    invert: Cell<bool>,
    /// Set while values are being loaded into the controls, so their change
    /// handlers do not write back what was just read.
    loading: Cell<bool>,
    /// Set during a drag: the plot's scale is frozen and the change is queued
    /// once, on release, rather than sixty times a second.
    dragging: Cell<Option<Handle>>,
    /// Plot extents, recomputed whenever no drag is running.
    extent: Cell<(f64, f64)>,
    live: Live,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Handle {
    End,
    Deadzone,
    Bend,
}

impl Editor {
    fn info(&self) -> &'static AxisInfo {
        &AXES[self.axis.get()]
    }

    /// Recompute the plot extents so the whole curve and some margin fit.
    fn fit(&self) {
        if self.dragging.get().is_some() {
            return;
        }
        let r = self.resp.get();
        let info = self.info();
        let x = info.plot_x.max(r.input_max_deg * 1.15).min(info.input_max);
        let y = (r.output_max_deg * 1.2).max(x).max(1.0);
        self.extent.set((x, y));
    }
}

/// The exponents the slider and the bend handle offer: the useful part of
/// [`Curve::POWER_RANGE`], which a terminal can still go past.
const EXPONENT_RANGE: (f64, f64) = (0.3, 4.0);

/// The controls that mirror [`Editor::resp`], so a drag can move them.
struct Controls {
    deadzone: Adjustment,
    input: Adjustment,
    output: Adjustment,
    shape: gtk::DropDown,
    exponent: Adjustment,
    invert: gtk::CheckButton,
}

impl Controls {
    /// Put the editor's current response into the controls without writing.
    fn show(&self, ed: &Editor) {
        ed.loading.set(true);
        let info = ed.info();
        let r = ed.resp.get();
        show_widened(&self.deadzone, (0.0, info.deadzone_max), r.deadzone_deg);
        show_widened(&self.input, (1.0, info.input_max), r.input_max_deg);
        show_widened(&self.output, (1.0, info.output_max), r.output_max_deg);
        let (sel, exp) = match r.curve {
            Curve::Linear => (0, 1.0),
            Curve::Smoothstep => (1, 1.0),
            Curve::Power(p) => (2, p),
        };
        self.shape.set_selected(sel);
        show_widened(&self.exponent, EXPONENT_RANGE, exp);
        self.invert.set_active(ed.invert.get());
        ed.loading.set(false);
    }
}

/// The curve editor: its state, its controls and its plot.
///
/// Held strongly by the window's re-read alone, and weakly by every handler in
/// it, for [`Ctx`]'s reason: the handlers belong to the widgets this holds.
struct Curves {
    ed: Editor,
    ctl: Controls,
    area: gtk::DrawingArea,
    units: Vec<Label>,
    ctx: Weak<Ctx>,
}

impl Curves {
    /// The plot's geometry at its current size.
    fn geometry(&self) -> Geometry {
        Geometry::new(
            self.area.width() as f64,
            self.area.height() as f64,
            self.ed.extent.get(),
        )
    }

    /// Load axis `i` from `cfg` into the editor, its controls and the plot.
    fn load(&self, cfg: &OutputConfig, i: usize) {
        self.ed.axis.set(i);
        self.ed.resp.set(cfg.head_curves.axes[i]);
        self.ed.invert.set(cfg.head_curves.invert[i]);
        self.ed.fit();
        self.ctl.show(&self.ed);
        for l in &self.units {
            l.set_text(self.ed.info().unit);
        }
        self.area.queue_draw();
    }

    /// Every control change: update the response, refit, redraw, save.
    fn changed(&self) {
        if self.ed.loading.get() {
            return;
        }
        let mut r = self.ed.resp.get();
        r.deadzone_deg = self.ctl.deadzone.value();
        r.input_max_deg = self.ctl.input.value().max(r.deadzone_deg + 0.5);
        r.output_max_deg = self.ctl.output.value();
        r.curve = match self.ctl.shape.selected() {
            1 => Curve::Smoothstep,
            2 => Curve::Power(self.ctl.exponent.value()),
            _ => Curve::Linear,
        };
        self.ed.resp.set(r);
        self.ed.invert.set(self.ctl.invert.is_active());
        self.ed.fit();
        self.save();
        self.area.queue_draw();
    }

    /// Queue this axis's curve and invert flag; on release, during a drag.
    fn save(&self) {
        if self.ed.dragging.get().is_some() {
            return;
        }
        let i = self.ed.axis.get();
        let r = self.ed.resp.get();
        let inv = self.ed.invert.get();
        if let Some(ctx) = self.ctx.upgrade() {
            ctx.queue(move |c| {
                let a = &mut c.head_curves.axes[i];
                a.deadzone_deg = r.deadzone_deg;
                a.input_max_deg = r.input_max_deg;
                a.output_max_deg = r.output_max_deg;
                a.curve = r.curve;
                c.head_curves.invert[i] = inv;
            });
        }
    }
}

/// Run `f` with the editor, if it is still there.
fn with(curves: &Weak<Curves>, f: impl FnOnce(&Curves)) {
    if let Some(c) = curves.upgrade() {
        f(&c);
    }
}

fn curves_section(ctx: &Rc<Ctx>, live: Live) -> gtk::Box {
    let b = card(
        "Response curves",
        "How far the game's view moves for each movement of your head. Drag the \
         end point for range and gain, the floor handle for the deadzone and the \
         middle handle to bend the curve. Negative movements mirror the positive \
         half. The dot is where your head is right now.",
    );

    // Axis picker.
    let picker = gtk::Box::new(Orientation::Horizontal, 0);
    picker.add_css_class("linked");
    let mut toggles: Vec<gtk::ToggleButton> = Vec::new();
    for info in &AXES {
        let t = crate::widget::toggle_button(info.title);
        if let Some(first) = toggles.first() {
            t.set_group(Some(first));
        }
        picker.append(&t);
        toggles.push(t);
    }
    toggles[0].set_active(true);
    b.append(&picker);

    let area = gtk::DrawingArea::new();
    area.set_content_height(320);
    area.set_hexpand(true);
    b.append(&area);

    let g = grid();
    // Steps only: `Controls::show` sets every range and value for the axis.
    let step = |step: f64| Adjustment::new(0.0, 0.0, 0.0, step, step * 10.0, 0.0);
    let (deadzone, input, output, exponent) = (step(0.5), step(1.0), step(1.0), step(0.05));
    let units = vec![
        slider(&g, 0, &DEADZONE, &deadzone, 1, ""),
        slider(&g, 1, &INPUT, &input, 0, ""),
        slider(&g, 2, &OUTPUT, &output, 0, ""),
    ];
    let shape_row = gtk::Box::new(Orientation::Horizontal, 10);
    shape_row.append(&Label::new(Some("Shape")));
    let shape = gtk::DropDown::from_strings(&["Linear", "Smoothstep (S-curve)", "Power"]);
    shape_row.append(&shape);
    let invert = gtk::CheckButton::with_label("Invert this axis");
    invert.set_hexpand(true);
    invert.set_halign(Align::End);
    shape_row.append(&invert);
    g.attach(&shape_row, 0, 3, 4, 1);
    slider(&g, 4, &EXPONENT, &exponent, 2, "");
    b.append(&g);

    let reset = crate::widget::button("Reset this axis");
    let make_linear = crate::widget::button("1:1 (no curve)");
    let buttons = gtk::Box::new(Orientation::Horizontal, 8);
    buttons.append(&make_linear);
    buttons.append(&reset);
    b.append(&buttons);

    // Neutral until the window's first re-read loads axis 0 into it.
    let curves = Rc::new(Curves {
        ed: Editor {
            axis: Cell::new(0),
            resp: Cell::new(OutputConfig::default().head_curves.axes[0]),
            invert: Cell::new(false),
            loading: Cell::new(false),
            dragging: Cell::new(None),
            extent: Cell::new((1.0, 1.0)),
            live,
        },
        ctl: Controls {
            deadzone,
            input,
            output,
            shape,
            exponent,
            invert,
        },
        area,
        units,
        ctx: Rc::downgrade(ctx),
    });
    {
        let curves = curves.clone();
        // Not under a drag: its release queues what the plot shows, and a
        // re-read would have put the file's curve back under the pointer.
        ctx.on_reload(move |c| {
            if curves.ed.dragging.get().is_none() {
                curves.load(c, curves.ed.axis.get());
            }
        });
    }
    let (c, weak) = (&*curves, Rc::downgrade(&curves));

    for a in [&c.ctl.deadzone, &c.ctl.input, &c.ctl.output] {
        let weak = weak.clone();
        a.connect_value_changed(move |_| with(&weak, Curves::changed));
    }
    {
        let weak = weak.clone();
        c.ctl.exponent.connect_value_changed(move |_| {
            with(&weak, |c| {
                if c.ed.loading.get() {
                    return;
                }
                // Moving the exponent is asking for a power curve.
                if c.ctl.shape.selected() != 2 {
                    c.ctl.shape.set_selected(2);
                }
                c.changed();
            })
        });
    }
    {
        let weak = weak.clone();
        c.ctl
            .shape
            .connect_selected_notify(move |_| with(&weak, Curves::changed));
    }
    {
        let weak = weak.clone();
        c.ctl
            .invert
            .connect_toggled(move |_| with(&weak, Curves::changed));
    }

    // Axis switch: the window's re-read, which writes what is queued for the
    // axis being left and loads the one now picked.
    for (i, t) in toggles.iter().enumerate() {
        let weak = weak.clone();
        t.connect_toggled(move |t| {
            if !t.is_active() {
                return;
            }
            with(&weak, |c| {
                c.ed.axis.set(i);
                if let Some(ctx) = c.ctx.upgrade() {
                    ctx.sync();
                }
            })
        });
    }

    {
        let weak = weak.clone();
        reset.connect_clicked(move |_| {
            with(&weak, |c| {
                let d = OutputConfig::default();
                let i = c.ed.axis.get();
                c.ed.resp.set(d.head_curves.axes[i]);
                c.ed.invert.set(DEFAULT_INVERT[i]);
                c.ctl.show(&c.ed);
                c.changed();
            })
        });
    }
    {
        let weak = weak.clone();
        make_linear.connect_clicked(move |_| {
            with(&weak, |c| {
                let mut r = c.ed.resp.get();
                r.deadzone_deg = 0.0;
                r.output_max_deg = r.input_max_deg;
                r.curve = Curve::Linear;
                c.ed.resp.set(r);
                c.ctl.show(&c.ed);
                c.changed();
            })
        });
    }

    // Drawing.
    {
        let weak = weak.clone();
        c.area.set_draw_func(move |_, cr, w, h| {
            with(&weak, |c| draw_curve(cr, w as f64, h as f64, &c.ed))
        });
    }

    // Dragging.
    let drag = gtk::GestureDrag::new();
    {
        let weak = weak.clone();
        drag.connect_drag_begin(move |_, x, y| {
            with(&weak, |c| {
                let geo = c.geometry();
                let r = c.ed.resp.get();
                let handles = [
                    (Handle::End, geo.pt(r.input_max_deg, r.output_max_deg)),
                    (Handle::Deadzone, geo.pt(r.deadzone_deg, 0.0)),
                    (Handle::Bend, bend_point(&geo, &r)),
                ];
                let near = handles
                    .iter()
                    .map(|(h, (px, py))| (*h, (px - x).hypot(py - y)))
                    .filter(|(_, d)| *d < 18.0)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(h, _)| h);
                c.ed.dragging.set(near);
            })
        });
    }
    {
        let weak = weak.clone();
        drag.connect_drag_update(move |g, dx, dy| {
            with(&weak, |c| {
                let Some(handle) = c.ed.dragging.get() else {
                    return;
                };
                let Some((sx, sy)) = g.start_point() else {
                    return;
                };
                let geo = c.geometry();
                let (vx, vy) = geo.value(sx + dx, sy + dy);
                let info = c.ed.info();
                let mut r = c.ed.resp.get();
                match handle {
                    Handle::End => {
                        r.input_max_deg = vx.clamp(r.deadzone_deg + 0.5, info.input_max);
                        r.output_max_deg = vy.clamp(1.0, info.output_max);
                    }
                    Handle::Deadzone => {
                        r.deadzone_deg =
                            vx.clamp(0.0, (r.input_max_deg - 0.5).min(info.deadzone_max));
                    }
                    Handle::Bend => {
                        // The curve passes through (0.5, v) of its span: power
                        // p with 0.5^p = v.
                        let v = (vy / r.output_max_deg).clamp(0.03, 0.97);
                        let p = (v.ln() / 0.5f64.ln()).clamp(EXPONENT_RANGE.0, EXPONENT_RANGE.1);
                        r.curve = Curve::Power((p * 100.0).round() / 100.0);
                    }
                }
                c.ed.resp.set(r);
                c.ctl.show(&c.ed);
                c.area.queue_draw();
            })
        });
    }
    {
        let weak = weak.clone();
        drag.connect_drag_end(move |_, _, _| {
            with(&weak, |c| {
                if c.ed.dragging.take().is_some() {
                    c.ed.fit();
                    c.save();
                    c.area.queue_draw();
                }
            })
        });
    }
    c.area.add_controller(drag);

    // The live marker: redrawn at the hub's tick while the window is open.
    {
        let weak = c.area.downgrade();
        glib::timeout_add_local(Duration::from_millis(33), move || match weak.upgrade() {
            Some(a) => {
                if a.is_mapped() {
                    a.queue_draw();
                }
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
    }

    b
}

/// Plot geometry: value space to pixels and back.
struct Geometry {
    x0: f64,
    y0: f64,
    w: f64,
    h: f64,
    xmax: f64,
    ymax: f64,
}

impl Geometry {
    fn new(width: f64, height: f64, (xmax, ymax): (f64, f64)) -> Geometry {
        let (left, right, top, bottom) = (52.0, 16.0, 14.0, 34.0);
        Geometry {
            x0: left,
            y0: height - bottom,
            w: (width - left - right).max(1.0),
            h: (height - top - bottom).max(1.0),
            xmax: xmax.max(1e-6),
            ymax: ymax.max(1e-6),
        }
    }
    fn pt(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.x0 + x / self.xmax * self.w,
            self.y0 - y / self.ymax * self.h,
        )
    }
    fn value(&self, px: f64, py: f64) -> (f64, f64) {
        (
            (px - self.x0) / self.w * self.xmax,
            (self.y0 - py) / self.h * self.ymax,
        )
    }
}

fn bend_point(geo: &Geometry, r: &AxisResponse) -> (f64, f64) {
    let x = r.deadzone_deg + 0.5 * (r.input_max_deg - r.deadzone_deg);
    geo.pt(x, r.shape(x))
}

/// A tick step giving four to eight gridlines over `max`.
fn tick_step(max: f64) -> f64 {
    let raw = max / 6.0;
    let mag = 10f64.powf(raw.log10().floor());
    for m in [1.0, 2.0, 5.0, 10.0] {
        if raw <= m * mag {
            return m * mag;
        }
    }
    10.0 * mag
}

fn draw_curve(cr: &cairo::Context, w: f64, h: f64, ed: &Editor) {
    let geo = Geometry::new(w, h, ed.extent.get());
    let r = ed.resp.get();
    let info = ed.info();
    let (xmax, ymax) = (geo.xmax, geo.ymax);

    cr.set_source_rgb(0.07, 0.08, 0.10);
    cr.rectangle(geo.x0, geo.y0 - geo.h, geo.w, geo.h);
    let _ = cr.fill();

    // Grid and labels.
    cr.set_font_size(11.0);
    cr.set_line_width(1.0);
    let xs = tick_step(xmax);
    let ys = tick_step(ymax);
    let mut t = 0.0;
    while t <= xmax + 1e-9 {
        let (px, _) = geo.pt(t, 0.0);
        cr.set_source_rgb(0.15, 0.17, 0.20);
        cr.move_to(px.floor() + 0.5, geo.y0);
        cr.line_to(px.floor() + 0.5, geo.y0 - geo.h);
        let _ = cr.stroke();
        cr.set_source_rgb(0.55, 0.58, 0.62);
        cr.move_to(px - 8.0, geo.y0 + 14.0);
        let _ = cr.show_text(&format!("{t:.0}"));
        t += xs;
    }
    let mut t = 0.0;
    while t <= ymax + 1e-9 {
        let (_, py) = geo.pt(0.0, t);
        cr.set_source_rgb(0.15, 0.17, 0.20);
        cr.move_to(geo.x0, py.floor() + 0.5);
        cr.line_to(geo.x0 + geo.w, py.floor() + 0.5);
        let _ = cr.stroke();
        cr.set_source_rgb(0.55, 0.58, 0.62);
        cr.move_to(4.0, py + 4.0);
        let _ = cr.show_text(&format!("{t:.0}"));
        t += ys;
    }
    cr.set_source_rgb(0.55, 0.58, 0.62);
    cr.move_to(geo.x0 + geo.w - 150.0, geo.y0 + 28.0);
    let _ = cr.show_text(&format!("your head ({})", info.unit));
    cr.move_to(geo.x0 + 4.0, geo.y0 - geo.h + 12.0);
    let _ = cr.show_text(&format!("game ({})", info.unit));

    // Deadzone band.
    if r.deadzone_deg > 0.0 {
        let (px, _) = geo.pt(r.deadzone_deg, 0.0);
        cr.set_source_rgba(0.9, 0.5, 0.2, 0.12);
        cr.rectangle(geo.x0, geo.y0 - geo.h, px - geo.x0, geo.h);
        let _ = cr.fill();
    }

    // 1:1 reference.
    cr.set_source_rgb(0.30, 0.33, 0.37);
    cr.set_dash(&[4.0, 4.0], 0.0);
    let m = xmax.min(ymax);
    let (ax, ay) = geo.pt(0.0, 0.0);
    let (bx, by) = geo.pt(m, m);
    cr.move_to(ax, ay);
    cr.line_to(bx, by);
    let _ = cr.stroke();
    cr.set_dash(&[], 0.0);

    // The curve.
    cr.set_source_rgb(0.35, 0.70, 1.0);
    cr.set_line_width(2.5);
    let n = 240;
    for i in 0..=n {
        let x = xmax * i as f64 / n as f64;
        let (px, py) = geo.pt(x, r.shape(x).min(ymax));
        if i == 0 {
            cr.move_to(px, py);
        } else {
            cr.line_to(px, py);
        }
    }
    let _ = cr.stroke();

    // Handles.
    let handle = |(x, y): (f64, f64), active: bool| {
        cr.arc(
            x,
            y,
            if active { 8.0 } else { 6.5 },
            0.0,
            std::f64::consts::TAU,
        );
        cr.set_source_rgb(0.95, 0.95, 0.97);
        let _ = cr.fill_preserve();
        cr.set_source_rgb(0.35, 0.70, 1.0);
        cr.set_line_width(2.0);
        let _ = cr.stroke();
    };
    let d = ed.dragging.get();
    handle(
        geo.pt(r.input_max_deg, r.output_max_deg),
        d == Some(Handle::End),
    );
    handle(geo.pt(r.deadzone_deg, 0.0), d == Some(Handle::Deadzone));
    handle(bend_point(&geo, &r), d == Some(Handle::Bend));

    // Live marker.
    if let Some(p) = (ed.live)() {
        let v = axis_value(&p, ed.axis.get());
        let a = v.abs();
        let out = r.shape(a);
        let (px, py) = geo.pt(a.min(xmax), out.min(ymax));
        cr.set_source_rgba(0.4, 0.9, 0.5, 0.5);
        cr.set_line_width(1.0);
        cr.move_to(px, geo.y0);
        cr.line_to(px, py);
        let _ = cr.stroke();
        cr.set_source_rgb(0.4, 0.9, 0.5);
        cr.arc(px, py, 5.0, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
        let sign = if ed.invert.get() {
            -v.signum()
        } else {
            v.signum()
        };
        cr.move_to(geo.x0 + geo.w - 230.0, geo.y0 - geo.h + 14.0);
        let _ = cr.show_text(&format!(
            "now: {v:+.1}{u} -> {:+.1}{u}",
            sign * out,
            u = info.unit
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tobii_output::curves::AXIS_NAMES;

    #[test]
    fn the_editor_axes_are_in_the_config_order() {
        assert_eq!(AXES.len(), AXIS_NAMES.len());
        let p = HeadPose {
            yaw_deg: 1.0,
            pitch_deg: 2.0,
            roll_deg: 3.0,
            x_mm: 4.0,
            y_mm: 5.0,
            z_mm: 6.0,
        };
        for i in 0..6 {
            assert_eq!(axis_value(&p, i), (i + 1) as f64);
        }
    }

    #[test]
    fn pixels_and_values_round_trip() {
        let g = Geometry::new(600.0, 320.0, (45.0, 90.0));
        let (px, py) = g.pt(30.0, 60.0);
        let (x, y) = g.value(px, py);
        assert!((x - 30.0).abs() < 1e-9 && (y - 60.0).abs() < 1e-9);
    }

    #[test]
    fn ticks_are_round_numbers() {
        assert_eq!(tick_step(45.0), 10.0);
        assert_eq!(tick_step(180.0), 50.0);
        assert_eq!(tick_step(6.0), 1.0);
    }
}
