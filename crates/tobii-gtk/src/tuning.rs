//! The "Tuning" window: gaze, smoothing and the per-axis response curves,
//! editable while the head is being tracked.
//!
//! Everything here is a `games.toml` key that `tobii games set` can also reach;
//! this window is the version of those keys a person can tune by feel. Each
//! control writes its own field through [`edit_config`], which re-reads the
//! file first, so this window, the Games row and a terminal never overwrite
//! each other's changes. The hub re-reads the file once a second, so a change
//! is felt in game a moment after it is made.
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
use std::rc::Rc;
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

thread_local! {
    /// The open window, so a second click raises it rather than opening a twin
    /// that would fight the first over the same keys.
    static OPEN: RefCell<Option<glib::WeakRef<gtk::Window>>> = const { RefCell::new(None) };
}

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
    AxisInfo { title: "Yaw", unit: "°", deadzone_max: 20.0, input_max: 180.0, output_max: 180.0, plot_x: 45.0 },
    AxisInfo { title: "Pitch", unit: "°", deadzone_max: 20.0, input_max: 90.0, output_max: 180.0, plot_x: 30.0 },
    AxisInfo { title: "Roll", unit: "°", deadzone_max: 20.0, input_max: 180.0, output_max: 180.0, plot_x: 30.0 },
    AxisInfo { title: "X (sideways)", unit: "mm", deadzone_max: 100.0, input_max: 500.0, output_max: 1000.0, plot_x: 150.0 },
    AxisInfo { title: "Y (up/down)", unit: "mm", deadzone_max: 100.0, input_max: 500.0, output_max: 1000.0, plot_x: 120.0 },
    AxisInfo { title: "Z (forward/back)", unit: "mm", deadzone_max: 100.0, input_max: 500.0, output_max: 1000.0, plot_x: 150.0 },
];

/// The value of axis `i` in `p`, in the order of [`AXIS_NAMES`].
fn axis_value(p: &HeadPose, i: usize) -> f64 {
    [p.yaw_deg, p.pitch_deg, p.roll_deg, p.x_mm, p.y_mm, p.z_mm][i]
}

/// Open the window, or raise it if it is already open.
pub fn open(anchor: &impl IsA<gtk::Widget>, live: Live) {
    if let Some(w) = OPEN.with(|o| o.borrow().as_ref().and_then(|w| w.upgrade())) {
        w.present();
        return;
    }
    let win = gtk::Window::new();
    win.set_title(Some("Head tracking tuning"));
    win.set_default_size(820, 760);
    if let Some(root) = anchor.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
        win.set_transient_for(Some(&root));
    }

    let cfg = load_output_config();
    let body = gtk::Box::new(Orientation::Vertical, 12);
    body.set_margin_top(16);
    body.set_margin_bottom(16);
    body.set_margin_start(16);
    body.set_margin_end(16);
    body.append(&curves_section(&cfg, live));
    body.append(&smoothing_section(&cfg));
    body.append(&gaze_section(&cfg));

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroll.set_child(Some(&body));
    win.set_child(Some(&scroll));

    OPEN.with(|o| *o.borrow_mut() = Some(win.downgrade()));
    win.connect_close_request(|_| {
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

/// A labelled slider with a spin button on the same adjustment.
///
/// `on_change` runs on every user change; programmatic `set_value` calls run it
/// too, so callers that load values guard with their own flag.
fn slider(
    grid: &gtk::Grid,
    row: i32,
    name: &str,
    adj: &Adjustment,
    digits: u32,
    unit: &str,
    tooltip: &str,
) {
    let l = Label::new(Some(name));
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
    for w in [l.upcast_ref::<gtk::Widget>(), scale.upcast_ref(), spin.upcast_ref()] {
        w.set_tooltip_text(Some(tooltip));
    }
    grid.attach(&l, 0, row, 1, 1);
    grid.attach(&scale, 1, row, 1, 1);
    grid.attach(&spin, 2, row, 1, 1);
    grid.attach(&u, 3, row, 1, 1);
}

fn grid() -> gtk::Grid {
    let g = gtk::Grid::new();
    g.set_column_spacing(10);
    g.set_row_spacing(4);
    g
}

/// An adjustment whose every change is written to the config by `write`.
fn bound(
    value: f64,
    lower: f64,
    upper: f64,
    step: f64,
    what: &'static str,
    write: impl Fn(&mut OutputConfig, f64) + 'static,
) -> Adjustment {
    let adj = Adjustment::new(value.clamp(lower, upper), lower, upper, step, step * 10.0, 0.0);
    adj.connect_value_changed(move |a| {
        let v = a.value();
        edit_config(what, |c| write(c, v));
    });
    adj
}

// --- Gaze ------------------------------------------------------------------

fn gaze_section(cfg: &OutputConfig) -> gtk::Box {
    let b = card(
        "Gaze (Extended View)",
        "Turns the camera further when you look towards the edge of the screen. \
         Off, the view follows your head alone — steadier, since eye movements are \
         fast and jumpy.",
    );
    let on = gtk::CheckButton::with_label("Gaze steers the camera");
    on.set_active(cfg.extended_view.enabled);
    b.append(&on);

    let g = grid();
    let yaw = bound(cfg.extended_view.yaw.output_max_deg, 0.0, 90.0, 1.0, "gaze yaw", |c, v| {
        c.extended_view.yaw.output_max_deg = v
    });
    let pitch = bound(cfg.extended_view.pitch.output_max_deg, 0.0, 60.0, 1.0, "gaze pitch", |c, v| {
        c.extended_view.pitch.output_max_deg = v
    });
    slider(&g, 0, "Sideways reach", &yaw, 0, "°", "How far the view turns when you look at the left or right edge");
    slider(&g, 1, "Vertical reach", &pitch, 0, "°", "How far the view tilts when you look at the top or bottom edge");
    g.set_sensitive(cfg.extended_view.enabled);
    b.append(&g);
    on.connect_toggled(move |c| {
        let v = c.is_active();
        edit_config("gaze setting", |cfg| cfg.extended_view.enabled = v);
        g.set_sensitive(v);
    });
    b
}

// --- Smoothing -------------------------------------------------------------

fn smoothing_section(cfg: &OutputConfig) -> gtk::Box {
    let b = card(
        "Smoothing",
        "Adaptive smooths hard while your head is still and lets a deliberate turn \
         through quickly. Fixed smooths everything by the same amount. The speed \
         limits stop sudden jumps from reaching the game.",
    );

    let mode_row = gtk::Box::new(Orientation::Horizontal, 10);
    mode_row.append(&Label::new(Some("Filter")));
    let mode = gtk::DropDown::from_strings(&["Adaptive (One-Euro)", "Fixed (EMA)"]);
    mode.set_selected(match cfg.filter_mode {
        FilterMode::Adaptive => 0,
        FilterMode::Ema => 1,
    });
    mode_row.append(&mode);
    b.append(&mode_row);

    let adaptive = grid();
    let cutoff = bound(cfg.filter_min_cutoff_hz, 0.05, 3.0, 0.05, "filter cutoff", |c, v| {
        c.filter_min_cutoff_hz = v
    });
    let beta = bound(cfg.filter_beta, 0.0, 0.2, 0.001, "filter speed response", |c, v| {
        c.filter_beta = v
    });
    slider(&adaptive, 0, "Steadiness at rest", &cutoff, 2, "Hz",
        "Cutoff while the head is still. Lower = steadier, but slower to settle");
    slider(&adaptive, 1, "Speed response", &beta, 3, "",
        "How quickly the filter opens up as you move. Higher = less lag on fast turns, \
         but noise can open it too");
    b.append(&adaptive);

    let ema = grid();
    let alpha = bound(cfg.filter_alpha, 0.02, 1.0, 0.01, "filter strength", |c, v| {
        c.filter_alpha = v
    });
    slider(&ema, 0, "Follow strength", &alpha, 2, "",
        "Weight of each new sample. Lower = smoother and laggier; 1 = no smoothing");
    b.append(&ema);

    let sel = |m: u32, a: &gtk::Grid, e: &gtk::Grid| {
        a.set_visible(m == 0);
        e.set_visible(m == 1);
    };
    sel(mode.selected(), &adaptive, &ema);
    mode.connect_selected_notify(move |d| {
        let m = d.selected();
        edit_config("filter mode", |c| {
            c.filter_mode = if m == 1 { FilterMode::Ema } else { FilterMode::Adaptive }
        });
        sel(m, &adaptive, &ema);
    });

    let limits = grid();
    let rot = bound(cfg.filter_max_rot_speed_deg_s, 20.0, 2000.0, 10.0, "rotation speed limit", |c, v| {
        c.filter_max_rot_speed_deg_s = v
    });
    let mov = bound(cfg.filter_max_move_speed_mm_s, 20.0, 3000.0, 10.0, "movement speed limit", |c, v| {
        c.filter_max_move_speed_mm_s = v
    });
    let jump = bound(cfg.filter_max_step_mm, 20.0, 1000.0, 5.0, "jump limit", |c, v| {
        c.filter_max_step_mm = v
    });
    slider(&limits, 0, "Max turn speed", &rot, 0, "°/s",
        "The fastest the output may rotate. Lower turns jumps into sweeps; too low feels sluggish");
    slider(&limits, 1, "Max move speed", &mov, 0, "mm/s", "The fastest the output may translate");
    slider(&limits, 2, "Glitch gate", &jump, 0, "mm",
        "A position step larger than this in one frame is treated as a tracking glitch and held");
    b.append(&limits);

    let ac = gtk::CheckButton::with_label("Auto-centre");
    ac.set_active(cfg.auto_centre.enabled);
    ac.set_tooltip_text(Some(crate::games::AUTO_CENTRE_TOOLTIP));
    b.append(&ac);
    let acg = grid();
    let t_rot = bound(cfg.auto_centre.time_s, 0.5, 60.0, 0.5, "auto-centre time", |c, v| {
        c.auto_centre.time_s = v
    });
    let t_pos = bound(cfg.auto_centre.translation_time_s, 0.5, 60.0, 0.5, "auto-centre position time", |c, v| {
        c.auto_centre.translation_time_s = v
    });
    let win = bound(cfg.auto_centre.window_deg, 2.0, 45.0, 1.0, "auto-centre window", |c, v| {
        c.auto_centre.window_deg = v
    });
    slider(&acg, 0, "Rotation pace", &t_rot, 1, "s", "How long rotation takes to drift back to centre. Lower = stronger pull");
    slider(&acg, 1, "Position pace", &t_pos, 1, "s", "The same for leaning. Lower = stronger pull back to centre");
    slider(&acg, 2, "Ignore turns past", &win, 0, "°", "A turn bigger than this is deliberate and is not pulled back");
    acg.set_sensitive(cfg.auto_centre.enabled);
    b.append(&acg);
    ac.connect_toggled(move |c| {
        let v = c.is_active();
        edit_config("auto-centre setting", |cfg| cfg.auto_centre.enabled = v);
        acg.set_sensitive(v);
    });

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
        reset.connect_clicked(move |_| {
            mode.set_selected(0);
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
    /// Set during a drag: the plot's scale is frozen and the file is written
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

    /// Write this axis's curve and invert flag to the config.
    fn save(&self) {
        if self.loading.get() || self.dragging.get().is_some() {
            return;
        }
        let i = self.axis.get();
        let r = self.resp.get();
        let inv = self.invert.get();
        edit_config("head curve", |c| {
            let a = &mut c.head_curves.axes[i];
            a.deadzone_deg = r.deadzone_deg;
            a.input_max_deg = r.input_max_deg;
            a.output_max_deg = r.output_max_deg;
            a.curve = r.curve;
            c.head_curves.invert[i] = inv;
        });
    }
}

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
        self.deadzone.set_upper(info.deadzone_max);
        self.input.set_upper(info.input_max);
        self.output.set_upper(info.output_max);
        self.deadzone.set_value(r.deadzone_deg);
        self.input.set_value(r.input_max_deg);
        self.output.set_value(r.output_max_deg);
        let (sel, exp) = match r.curve {
            Curve::Linear => (0, 1.0),
            Curve::Smoothstep => (1, 1.0),
            Curve::Power(p) => (2, p),
        };
        self.shape.set_selected(sel);
        self.exponent.set_value(exp);
        self.invert.set_active(ed.invert.get());
        ed.loading.set(false);
    }
}

fn curves_section(cfg: &OutputConfig, live: Live) -> gtk::Box {
    let b = card(
        "Response curves",
        "How far the game's view moves for each movement of your head. Drag the \
         end point for range and gain, the floor handle for the deadzone and the \
         middle handle to bend the curve. Negative movements mirror the positive \
         half. The dot is where your head is right now.",
    );

    let ed = Rc::new(Editor {
        axis: Cell::new(0),
        resp: Cell::new(cfg.head_curves.axes[0]),
        invert: Cell::new(cfg.head_curves.invert[0]),
        loading: Cell::new(false),
        dragging: Cell::new(None),
        extent: Cell::new((1.0, 1.0)),
        live,
    });
    ed.fit();

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
    let deadzone = Adjustment::new(0.0, 0.0, 20.0, 0.5, 5.0, 0.0);
    let input = Adjustment::new(30.0, 1.0, 180.0, 1.0, 10.0, 0.0);
    let output = Adjustment::new(30.0, 1.0, 180.0, 1.0, 10.0, 0.0);
    let exponent = Adjustment::new(1.0, 0.3, 4.0, 0.05, 0.5, 0.0);
    slider(&g, 0, "Deadzone", &deadzone, 1, "", "Movement from centre that does nothing at all");
    slider(&g, 1, "Head movement for full output", &input, 0, "",
        "How far you move your head to reach the full output below");
    slider(&g, 2, "Full output", &output, 0, "", "What the game receives at that head movement");
    let shape_row = gtk::Box::new(Orientation::Horizontal, 10);
    shape_row.append(&Label::new(Some("Shape")));
    let shape = gtk::DropDown::from_strings(&["Linear", "Smoothstep (S-curve)", "Power"]);
    shape_row.append(&shape);
    let invert = gtk::CheckButton::with_label("Invert this axis");
    invert.set_hexpand(true);
    invert.set_halign(Align::End);
    shape_row.append(&invert);
    g.attach(&shape_row, 0, 3, 4, 1);
    slider(&g, 4, "Power exponent", &exponent, 2, "",
        "Above 1: gentle near centre, steep at the edge. Below 1: the opposite");
    b.append(&g);

    let reset = crate::widget::button("Reset this axis");
    let make_linear = crate::widget::button("1:1 (no curve)");
    let buttons = gtk::Box::new(Orientation::Horizontal, 8);
    buttons.append(&make_linear);
    buttons.append(&reset);
    b.append(&buttons);

    let ctl = Rc::new(Controls { deadzone, input, output, shape, exponent, invert });
    ctl.show(&ed);

    // Units in the slider rows follow the axis.
    let unit_labels: Vec<Label> = (0..3)
        .filter_map(|row| g.child_at(3, row).and_downcast::<Label>())
        .collect();
    let set_units = {
        let ed = ed.clone();
        move || {
            for l in &unit_labels {
                l.set_text(ed.info().unit);
            }
        }
    };
    set_units();

    // Every control change: update the response, refit, redraw, save.
    let changed: Rc<dyn Fn()> = {
        let ed = ed.clone();
        let ctl = ctl.clone();
        let area = area.clone();
        Rc::new(move || {
            if ed.loading.get() {
                return;
            }
            let mut r = ed.resp.get();
            r.deadzone_deg = ctl.deadzone.value();
            r.input_max_deg = ctl.input.value().max(r.deadzone_deg + 0.5);
            r.output_max_deg = ctl.output.value();
            r.curve = match ctl.shape.selected() {
                1 => Curve::Smoothstep,
                2 => Curve::Power(ctl.exponent.value()),
                _ => Curve::Linear,
            };
            ed.resp.set(r);
            ed.invert.set(ctl.invert.is_active());
            ed.fit();
            ed.save();
            area.queue_draw();
        })
    };
    for a in [&ctl.deadzone, &ctl.input, &ctl.output] {
        let changed = changed.clone();
        a.connect_value_changed(move |_| changed());
    }
    {
        let changed = changed.clone();
        let ctl2 = ctl.clone();
        let ed = ed.clone();
        ctl.exponent.connect_value_changed(move |_| {
            if ed.loading.get() {
                return;
            }
            // Moving the exponent is asking for a power curve.
            if ctl2.shape.selected() != 2 {
                ctl2.shape.set_selected(2);
            }
            changed();
        });
    }
    {
        let changed = changed.clone();
        ctl.shape.connect_selected_notify(move |_| changed());
    }
    {
        let changed = changed.clone();
        ctl.invert.connect_toggled(move |_| changed());
    }

    // Axis switch: load that axis from the file.
    for (i, t) in toggles.iter().enumerate() {
        let ed = ed.clone();
        let ctl = ctl.clone();
        let area = area.clone();
        let set_units = set_units.clone();
        t.connect_toggled(move |t| {
            if !t.is_active() {
                return;
            }
            let cfg = load_output_config();
            ed.axis.set(i);
            ed.resp.set(cfg.head_curves.axes[i]);
            ed.invert.set(cfg.head_curves.invert[i]);
            ed.fit();
            ctl.show(&ed);
            set_units();
            area.queue_draw();
        });
    }

    {
        let ed = ed.clone();
        let ctl = ctl.clone();
        let changed = changed.clone();
        reset.connect_clicked(move |_| {
            let d = OutputConfig::default();
            let i = ed.axis.get();
            ed.resp.set(d.head_curves.axes[i]);
            ed.invert.set(DEFAULT_INVERT[i]);
            ctl.show(&ed);
            changed();
        });
    }
    {
        let ed = ed.clone();
        let ctl = ctl.clone();
        let changed = changed.clone();
        make_linear.connect_clicked(move |_| {
            let mut r = ed.resp.get();
            r.deadzone_deg = 0.0;
            r.output_max_deg = r.input_max_deg;
            r.curve = Curve::Linear;
            ed.resp.set(r);
            ctl.show(&ed);
            changed();
        });
    }

    // Drawing.
    {
        let ed = ed.clone();
        area.set_draw_func(move |_, cr, w, h| draw_curve(cr, w as f64, h as f64, &ed));
    }

    // Dragging.
    let drag = gtk::GestureDrag::new();
    {
        let ed = ed.clone();
        let area = area.clone();
        drag.connect_drag_begin(move |_, x, y| {
            let geo = Geometry::new(area.width() as f64, area.height() as f64, ed.extent.get());
            let r = ed.resp.get();
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
            ed.dragging.set(near);
        });
    }
    {
        let ed = ed.clone();
        let ctl = ctl.clone();
        let area = area.clone();
        drag.connect_drag_update(move |g, dx, dy| {
            let Some(handle) = ed.dragging.get() else { return };
            let Some((sx, sy)) = g.start_point() else { return };
            let geo = Geometry::new(area.width() as f64, area.height() as f64, ed.extent.get());
            let (vx, vy) = geo.value(sx + dx, sy + dy);
            let info = ed.info();
            let mut r = ed.resp.get();
            match handle {
                Handle::End => {
                    r.input_max_deg = vx.clamp(r.deadzone_deg + 0.5, info.input_max);
                    r.output_max_deg = vy.clamp(1.0, info.output_max);
                }
                Handle::Deadzone => {
                    r.deadzone_deg = vx.clamp(0.0, (r.input_max_deg - 0.5).min(info.deadzone_max));
                }
                Handle::Bend => {
                    // The curve passes through (0.5, v) of its span: power p
                    // with 0.5^p = v.
                    let v = (vy / r.output_max_deg).clamp(0.03, 0.97);
                    let p = (v.ln() / 0.5f64.ln()).clamp(0.3, 4.0);
                    r.curve = Curve::Power((p * 100.0).round() / 100.0);
                }
            }
            ed.resp.set(r);
            ctl.show(&ed);
            area.queue_draw();
        });
    }
    {
        let ed = ed.clone();
        let area = area.clone();
        drag.connect_drag_end(move |_, _, _| {
            if ed.dragging.take().is_some() {
                ed.fit();
                ed.save();
                area.queue_draw();
            }
        });
    }
    area.add_controller(drag);

    // The live marker: redrawn at the hub's tick while the window is open.
    {
        let weak = area.downgrade();
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
        (self.x0 + x / self.xmax * self.w, self.y0 - y / self.ymax * self.h)
    }
    fn value(&self, px: f64, py: f64) -> (f64, f64) {
        ((px - self.x0) / self.w * self.xmax, (self.y0 - py) / self.h * self.ymax)
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
        cr.arc(x, y, if active { 8.0 } else { 6.5 }, 0.0, std::f64::consts::TAU);
        cr.set_source_rgb(0.95, 0.95, 0.97);
        let _ = cr.fill_preserve();
        cr.set_source_rgb(0.35, 0.70, 1.0);
        cr.set_line_width(2.0);
        let _ = cr.stroke();
    };
    let d = ed.dragging.get();
    handle(geo.pt(r.input_max_deg, r.output_max_deg), d == Some(Handle::End));
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
        let sign = if ed.invert.get() { -v.signum() } else { v.signum() };
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
