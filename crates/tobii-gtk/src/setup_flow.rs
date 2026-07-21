//! Fullscreen display-setup flow (the original's `-S`).
//!
//! The screen geometry is **seeded from the monitor's EDID** (`detect_monitors`)
//! whenever one can be detected: deriving the size purely from dragged lines is
//! error-prone, because width is *inversely* proportional to the line gap — a
//! user who dragged the lines to half the tracker's real width ended up with a
//! 2431 mm "screen" (real monitor: 1193 x 336 mm), which threw gaze mapping off
//! by centimetres. EDID makes that class of error nearly impossible.
//!
//! The two vertical lines are still draggable and keep their meaning (width +
//! horizontal offset via `align`), so absent or wrong EDID can be corrected by
//! eye. They are *rendered* from the current width/offset, so seeding the width
//! places them correctly for free. A "Show advanced" toggle reveals the editable
//! numeric form (compact −[value]+ spinners, two-way synced with the drag), each
//! row carrying a "?" button whose tooltip explains + diagrams the field.
//! Apply persists + pushes to the device; Cancel/Esc returns.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::Sender;

use gtk::prelude::*;
use gtk::{
    cairo, Align, Application, Button, DrawingArea, Entry, GestureDrag, Grid, Label, Orientation,
    Overlay, ToggleButton,
};

use crate::align;
use crate::device::DeviceCommand;
use crate::{add_escape_to_close, screen_height};
use tobii_config::{pick_monitor, DisplaySetup};

fn aspect(area: &DrawingArea) -> f64 {
    let w = area.width().max(1) as f64;
    let h = area.height().max(1) as f64;
    w / h
}

fn default_setup() -> DisplaySetup {
    DisplaySetup {
        width_mm: 600.0,
        height_mm: 340.0,
        tilt_deg: 20.0,
        offset_x_mm: 0.0,
        offset_y_mm: 10.0,
        offset_z_mm: 0.0,
        curvature_radius_mm: 0.0,
    }
}

fn fmt_val(v: f64) -> String {
    format!("{v:.0}")
}

type Getter = Rc<dyn Fn(&DisplaySetup) -> f64>;
type Setter = Rc<dyn Fn(&mut DisplaySetup, f64)>;

/// A late-bound "this field changed" hook. The curvature spinner needs to write
/// back into *another* field's entry, which does not exist until every spinner
/// has been built — so the spinner holds the cell and the closure is dropped in
/// afterwards.
type Hook = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// Run `hook`'s closure, if one has been installed. Clones it out of the
/// `RefCell` first: the closure re-enters the setup state, and this file has
/// shipped a double-borrow panic before (c8db2c7).
fn fire(hook: &Option<Hook>) {
    let f = hook.as_ref().and_then(|c| c.borrow().clone());
    if let Some(f) = f {
        f();
    }
}

/// One editable field: its entry + a getter for refreshing it from the setup.
struct Field {
    entry: Entry,
    get: Getter,
}

// ---------------------------------------------------------------------------
// Field help: a "?" button per row whose tooltip is a cairo diagram + a sentence.
// Line art only — this project ships no binary assets and must stay self-contained.
// ---------------------------------------------------------------------------

/// Highlighted dimension.
const TEAL: (f64, f64, f64) = (0.30, 0.85, 0.85);
/// Context geometry (the monitor/tracker outlines the dimension is measured on).
const GREY: (f64, f64, f64) = (0.55, 0.60, 0.66);

fn rgb(cr: &cairo::Context, c: (f64, f64, f64)) {
    cr.set_source_rgb(c.0, c.1, c.2);
}

/// Dark backdrop so the line art reads inside the tooltip.
fn diagram_bg(cr: &cairo::Context) {
    cr.set_source_rgb(0.08, 0.10, 0.12);
    let _ = cr.paint();
}

/// A filled arrow head at `(x, y)` pointing along the unit vector `(dx, dy)`.
fn arrow_head(cr: &cairo::Context, x: f64, y: f64, dx: f64, dy: f64) {
    let (s, t) = (7.0, 3.5); // length, half-width
    let (px, py) = (-dy, dx); // perpendicular
    cr.move_to(x, y);
    cr.line_to(x - dx * s + px * t, y - dy * s + py * t);
    cr.line_to(x - dx * s - px * t, y - dy * s - py * t);
    cr.close_path();
    let _ = cr.fill();
}

/// A teal double-headed arrow from `(x1, y1)` to `(x2, y2)`.
fn double_arrow(cr: &cairo::Context, x1: f64, y1: f64, x2: f64, y2: f64) {
    rgb(cr, TEAL);
    cr.set_line_width(1.6);
    cr.move_to(x1, y1);
    cr.line_to(x2, y2);
    let _ = cr.stroke();
    let (dx, dy) = (x2 - x1, y2 - y1);
    let len = (dx * dx + dy * dy).sqrt().max(1e-6);
    let (ux, uy) = (dx / len, dy / len);
    arrow_head(cr, x2, y2, ux, uy);
    arrow_head(cr, x1, y1, -ux, -uy);
}

/// Small teal caption centred on `x`.
fn caption(cr: &cairo::Context, x: f64, y: f64, text: &str) {
    rgb(cr, TEAL);
    cr.set_font_size(11.0);
    let w = cr.text_extents(text).map(|e| e.width()).unwrap_or(0.0);
    cr.move_to(x - w / 2.0, y);
    let _ = cr.show_text(text);
}

/// Grey monitor front view: bezel + inset glass. Returns the glass rectangle.
fn front_monitor(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64) -> (f64, f64, f64, f64) {
    rgb(cr, GREY);
    cr.set_line_width(1.4);
    cr.rectangle(x, y, w, h);
    let _ = cr.stroke();
    let g = (x + 8.0, y + 8.0, w - 16.0, h - 16.0);
    cr.set_line_width(1.0);
    cr.rectangle(g.0, g.1, g.2, g.3);
    let _ = cr.stroke();
    g
}

/// Grey tracker bar (front view: wide; side view: short) centred on `x`.
fn tracker_bar(cr: &cairo::Context, cx: f64, cy: f64, w: f64) {
    rgb(cr, GREY);
    cr.set_line_width(1.4);
    cr.rectangle(cx - w / 2.0, cy - 3.0, w, 6.0);
    let _ = cr.stroke();
}

/// Grey side view of a screen leaning back from its bottom edge at `(sx, sy)`.
fn side_screen(cr: &cairo::Context, sx: f64, sy: f64) {
    rgb(cr, GREY);
    cr.set_line_width(2.0);
    cr.move_to(sx, sy);
    cr.line_to(sx + 26.0, sy - 72.0);
    let _ = cr.stroke();
    // A stubby foot so the "bottom of the glass" end is unmistakable.
    cr.set_line_width(1.2);
    cr.move_to(sx - 10.0, sy + 8.0);
    cr.line_to(sx + 14.0, sy + 8.0);
    let _ = cr.stroke();
    cr.move_to(sx, sy);
    cr.line_to(sx + 4.0, sy + 8.0);
    let _ = cr.stroke();
}

fn diagram_width(cr: &cairo::Context) {
    diagram_bg(cr);
    let g = front_monitor(cr, 30.0, 24.0, 160.0, 82.0);
    double_arrow(cr, g.0, g.1 + g.3 / 2.0, g.0 + g.2, g.1 + g.3 / 2.0);
    caption(cr, 110.0, g.1 + g.3 / 2.0 - 8.0, "width");
}

fn diagram_height(cr: &cairo::Context) {
    diagram_bg(cr);
    let g = front_monitor(cr, 30.0, 24.0, 160.0, 82.0);
    double_arrow(cr, g.0 + g.2 / 2.0, g.1, g.0 + g.2 / 2.0, g.1 + g.3);
    caption(cr, 140.0, g.1 + g.3 / 2.0 + 4.0, "height");
}

fn diagram_tilt(cr: &cairo::Context) {
    diagram_bg(cr);
    let (sx, sy) = (85.0, 112.0);
    side_screen(cr, sx, sy);
    // Vertical dashed reference the tilt is measured against.
    rgb(cr, GREY);
    cr.set_line_width(1.0);
    cr.set_dash(&[3.0, 3.0], 0.0);
    cr.move_to(sx, sy);
    cr.line_to(sx, sy - 78.0);
    let _ = cr.stroke();
    cr.set_dash(&[], 0.0);
    // Arc from vertical to the screen line.
    rgb(cr, TEAL);
    cr.set_line_width(1.6);
    let r = 46.0;
    let lean = (26.0f64).atan2(72.0); // screen's lean from vertical
    let up = -std::f64::consts::FRAC_PI_2;
    cr.arc(sx, sy, r, up, up + lean);
    let _ = cr.stroke();
    caption(cr, sx + 40.0, sy - 46.0, "tilt");
}

fn diagram_offset_y(cr: &cairo::Context) {
    diagram_bg(cr);
    let (sx, sy) = (100.0, 84.0);
    side_screen(cr, sx, sy);
    let ty = 122.0;
    tracker_bar(cr, 60.0, ty, 34.0);
    // Where the screen bottom sits, carried across to the arrow.
    rgb(cr, GREY);
    cr.set_line_width(0.8);
    cr.set_dash(&[3.0, 3.0], 0.0);
    cr.move_to(60.0, sy);
    cr.line_to(sx, sy);
    let _ = cr.stroke();
    cr.set_dash(&[], 0.0);
    double_arrow(cr, 60.0, ty - 4.0, 60.0, sy);
    caption(cr, 60.0, sy - 8.0, "height");
}

fn diagram_offset_z(cr: &cairo::Context) {
    diagram_bg(cr);
    let (sx, sy) = (128.0, 96.0);
    side_screen(cr, sx, sy);
    let (tx, ty) = (56.0, 118.0);
    tracker_bar(cr, tx, ty, 34.0);
    // Drop the screen bottom down to the tracker's level to compare depth.
    rgb(cr, GREY);
    cr.set_line_width(0.8);
    cr.set_dash(&[3.0, 3.0], 0.0);
    cr.move_to(sx, sy);
    cr.line_to(sx, ty + 14.0);
    cr.move_to(tx, ty + 4.0);
    cr.line_to(tx, ty + 14.0);
    let _ = cr.stroke();
    cr.set_dash(&[], 0.0);
    double_arrow(cr, tx, ty + 14.0, sx, ty + 14.0);
    caption(cr, (tx + sx) / 2.0, ty + 30.0, "depth");
}

fn diagram_offset_x(cr: &cairo::Context) {
    diagram_bg(cr);
    // Monitor deliberately off-centre from the tracker below it.
    let g = front_monitor(cr, 52.0, 20.0, 140.0, 74.0);
    let scx = g.0 + g.2 / 2.0;
    let tcx = 88.0;
    tracker_bar(cr, tcx, 116.0, 60.0);
    rgb(cr, GREY);
    cr.set_line_width(0.8);
    cr.set_dash(&[3.0, 3.0], 0.0);
    cr.move_to(scx, g.1 + g.3);
    cr.line_to(scx, 106.0);
    cr.move_to(tcx, 112.0);
    cr.line_to(tcx, 106.0);
    let _ = cr.stroke();
    cr.set_dash(&[], 0.0);
    double_arrow(cr, tcx, 104.0, scx, 104.0);
    caption(cr, (tcx + scx) / 2.0, 98.0, "offset");
}

/// Top-down view of a curved screen: the arc, the straight chord across its
/// ends, the sagitta between them, and the radius struck from the viewer side.
fn diagram_curvature(cr: &cairo::Context) {
    diagram_bg(cr);
    // Centre of curvature sits on the viewer's side (below, in this top-down
    // view), so the screen bows away from the viewer.
    let (ccx, ccy) = (110.0, 240.0);
    let r = 190.0;
    let half = 0.55f64; // half-angle the drawn screen subtends
    let up = -std::f64::consts::FRAC_PI_2;

    // The screen arc.
    rgb(cr, GREY);
    cr.set_line_width(2.0);
    cr.arc(ccx, ccy, r, up - half, up + half);
    let _ = cr.stroke();

    // Its two ends, and the straight chord between them.
    let end = |s: f64| (ccx + r * (up + s).cos(), ccy + r * (up + s).sin());
    let (lx, ly) = end(-half);
    let (rx, ry) = end(half);
    cr.set_line_width(1.4);
    cr.set_dash(&[4.0, 3.0], 0.0);
    cr.move_to(lx, ly);
    cr.line_to(rx, ry);
    let _ = cr.stroke();
    cr.set_dash(&[], 0.0);

    // Radius, struck from the centre of curvature out to the screen.
    rgb(cr, GREY);
    cr.set_line_width(0.8);
    cr.set_dash(&[3.0, 3.0], 0.0);
    cr.move_to(ccx, ccy);
    cr.line_to(ccx, ccy - r);
    let _ = cr.stroke();
    cr.set_dash(&[], 0.0);
    caption(cr, ccx + 26.0, ccy - 34.0, "radius");

    // Sagitta: chord midpoint to the deepest point of the arc.
    double_arrow(cr, ccx, (ly + ry) / 2.0, ccx, ccy - r);
    caption(cr, ccx - 44.0, ccy - r + 16.0, "sagitta");
    caption(cr, rx - 22.0, ry + 16.0, "chord");
}

/// A "?" button whose tooltip is `diagram` over `text`.
fn help_button(diagram: fn(&cairo::Context), text: &'static str) -> Button {
    let btn = Button::with_label("?");
    btn.add_css_class("help-btn");
    btn.set_valign(Align::Center);
    btn.set_has_tooltip(true);
    btn.connect_query_tooltip(move |_, _, _, _, tooltip| {
        let area = DrawingArea::new();
        area.set_content_width(220);
        area.set_content_height(140);
        area.set_draw_func(move |_, cr, _, _| diagram(cr));

        let label = Label::new(Some(text));
        label.set_wrap(true);
        label.set_max_width_chars(38);
        label.set_xalign(0.0);

        let bx = gtk::Box::new(Orientation::Vertical, 8);
        bx.append(&area);
        bx.append(&label);
        tooltip.set_custom(Some(&bx));
        true
    });
    btn
}

/// Build + wire one compact `−[entry]+` spinner row into the grid.
#[allow(clippy::too_many_arguments)]
fn add_spinner(
    grid: &Grid,
    row: i32,
    label: &str,
    help: (fn(&cairo::Context), &'static str),
    step: f64,
    min: f64,
    max: f64,
    setup: &Rc<RefCell<DisplaySetup>>,
    syncing: &Rc<Cell<bool>>,
    refresh_view: &Rc<dyn Fn()>,
    get: Getter,
    set: Setter,
    hook: Option<Hook>,
) -> Field {
    let lbl = Label::new(Some(label));
    lbl.set_halign(Align::Start);
    lbl.add_css_class("section-desc");

    let minus = Button::with_label("−");
    minus.add_css_class("spin-btn");
    let entry = Entry::new();
    entry.set_width_chars(5);
    entry.set_max_width_chars(5);
    entry.add_css_class("spin-entry");
    let plus = Button::with_label("+");
    plus.add_css_class("spin-btn");

    let rowbox = gtk::Box::new(Orientation::Horizontal, 4);
    rowbox.append(&minus);
    rowbox.append(&entry);
    rowbox.append(&plus);
    grid.attach(&lbl, 0, row, 1, 1);
    grid.attach(&rowbox, 1, row, 1, 1);
    grid.attach(&help_button(help.0, help.1), 2, row, 1, 1);

    // Commit a value: clamp, store, reflect in the entry (guarded), refresh view.
    let commit: Rc<dyn Fn(f64)> = {
        let setup = setup.clone();
        let syncing = syncing.clone();
        let refresh_view = refresh_view.clone();
        let entry = entry.clone();
        let set = set.clone();
        let hook = hook.clone();
        Rc::new(move |v: f64| {
            let v = v.clamp(min, max);
            set(&mut setup.borrow_mut(), v); // borrow released before fire()
            syncing.set(true);
            entry.set_text(&fmt_val(v));
            syncing.set(false);
            fire(&hook);
            refresh_view();
        })
    };

    // Typing: parse + store + refresh (don't rewrite the entry mid-typing).
    {
        let setup = setup.clone();
        let syncing = syncing.clone();
        let refresh_view = refresh_view.clone();
        let set = set.clone();
        entry.connect_changed(move |e| {
            if syncing.get() {
                return;
            }
            if let Ok(v) = e.text().trim().parse::<f64>() {
                set(&mut setup.borrow_mut(), v.clamp(min, max)); // borrow released
                fire(&hook);
                refresh_view();
            }
        });
    }
    {
        let get = get.clone();
        let setup = setup.clone();
        let commit = commit.clone();
        minus.connect_clicked(move |_| {
            let v = get(&setup.borrow()) - step; // borrow released before commit()
            commit(v);
        });
    }
    {
        let get = get.clone();
        let setup = setup.clone();
        let commit = commit.clone();
        plus.connect_clicked(move |_| {
            let v = get(&setup.borrow()) + step; // borrow released before commit()
            commit(v);
        });
    }

    Field { entry, get }
}

/// Open the fullscreen display-setup flow window.
pub fn launch(app: &Application, cmd_tx: Sender<DeviceCommand>) {
    // Seed from EDID when we can: the saved/default config supplies the pose
    // (tilt + offsets), the detected monitor overrides the physical size, which
    // is the part users get catastrophically wrong when dragging the lines.
    let monitors = tobii_config::detect_monitors();
    let detected = pick_monitor(&monitors).cloned();
    let mut initial = tobii_config::load()
        .ok()
        .flatten()
        .unwrap_or_else(default_setup);
    // On a curved panel EDID reports the *arc* width (the panel is a flat sheet
    // bent into an arc), but `width_mm` is the straight chord the flat display
    // area is built from — so convert whenever a curve radius is configured.
    //
    // The radius here comes from the *saved* config, which a first-time user of
    // a curved monitor does not have: it is 0, the conversion is the identity,
    // and the arc gets seeded as if it were the chord. So keep the arc for the
    // lifetime of the flow and re-derive the width whenever the radius changes
    // (see `on_curve` below) — otherwise typing "1800" into the curvature
    // spinner would leave a 22 mm-too-wide screen on disk and on the device.
    let edid_arc_mm: Option<f64> = detected.as_ref().map(|m| m.width_mm);
    if let Some(m) = &detected {
        initial.width_mm = tobii_config::chord_from_arc(m.width_mm, initial.curvature_radius_mm);
        initial.height_mm = m.height_mm;
    }
    let setup = Rc::new(RefCell::new(initial));

    let win = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Set up display")
        .build();
    win.set_modal(true);
    win.fullscreen();

    // Full-screen drawing surface: the lines are derived from the setup.
    let area = DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    {
        let setup = setup.clone();
        area.set_draw_func(move |_, cr, w, h| {
            let s = *setup.borrow();
            let lines = align::lines_from_width_offset(s.width_mm, s.offset_x_mm);
            draw_align(cr, w, h, lines);
        });
    }

    // Header (instruction + readout + buttons) — anchored at a fixed spot a bit
    // above the middle, so revealing the advanced box below doesn't move it.
    let instr = Label::new(Some(
        "Drag the two lines to the marks on the top corners of your eye tracker.",
    ));
    instr.add_css_class("app-title");
    instr.set_halign(Align::Center);
    instr.set_wrap(true);
    instr.set_justify(gtk::Justification::Center);

    let readout = Label::new(Some(""));
    readout.add_css_class("guidance");
    readout.set_halign(Align::Center);

    // Detection status: reassurance when EDID worked, a call to action when not.
    let status = Label::new(None);
    status.add_css_class("section-desc");
    status.set_halign(Align::Center);
    status.set_wrap(true);
    status.set_justify(gtk::Justification::Center);
    status.set_max_width_chars(70);

    // Rebuilt from the current setup on every change, so the arc→chord
    // straightening becomes visible the moment a curve radius is entered, and a
    // physically impossible radius says so instead of silently doing nothing.
    let refresh_status: Rc<dyn Fn()> = {
        let setup = setup.clone();
        let status = status.clone();
        let detected = detected.clone();
        Rc::new(move || {
            let s = *setup.borrow(); // Copy; borrow released here
            let mut warn = false;
            let mut text = match &detected {
                Some(m) => {
                    let chord = tobii_config::chord_from_arc(m.width_mm, s.curvature_radius_mm);
                    if (chord - m.width_mm).abs() >= 0.5 {
                        format!(
                            "Detected {} — {:.0} × {:.0} mm. That width follows the curve; \
                             straightened for a {:.0}R screen it is {:.0} mm. Drag the lines \
                             only if this looks wrong.",
                            m.model, m.width_mm, m.height_mm, s.curvature_radius_mm, chord
                        )
                    } else {
                        format!(
                            "Detected {} — {:.0} × {:.0} mm. Drag the lines only if this \
                             looks wrong.",
                            m.model, m.width_mm, m.height_mm
                        )
                    }
                }
                None => {
                    warn = true;
                    "Could not detect your monitor. Measure the screen glass (not the bezel) \
                     and set the size under Show advanced."
                        .to_string()
                }
            };
            // No circle of this radius passes through both side edges, so the
            // curvature correction cannot run at all. Silently behaving like a
            // flat screen would be indistinguishable from typing 0.
            if s.curvature_radius_mm > 0.0 && s.curvature_radius_mm <= s.width_mm / 2.0 {
                warn = true;
                text.push_str(&format!(
                    "\nA {:.0} mm curve radius is impossible on a {:.0} mm wide screen — it \
                     has to be more than half the width ({:.0} mm), so the curve correction \
                     is switched off. Use the figure from the spec sheet: \"1800R\" is 1800.",
                    s.curvature_radius_mm,
                    s.width_mm,
                    s.width_mm / 2.0
                ));
            }
            if warn {
                status.add_css_class("section-warn");
            } else {
                status.remove_css_class("section-warn");
            }
            status.set_text(&text);
        })
    };

    let apply = Button::with_label("Apply & save");
    let advanced = ToggleButton::with_label("Show advanced");
    let cancel = Button::with_label("Cancel");
    let buttons = gtk::Box::new(Orientation::Horizontal, 10);
    buttons.set_halign(Align::Center);
    buttons.append(&apply);
    buttons.append(&advanced);
    buttons.append(&cancel);

    let corners = Label::new(Some(""));
    corners.add_css_class("section-desc");

    let syncing = Rc::new(Cell::new(false));

    let refresh_view: Rc<dyn Fn()> = {
        let setup = setup.clone();
        let area = area.clone();
        let readout = readout.clone();
        let corners = corners.clone();
        let refresh_status = refresh_status.clone();
        Rc::new(move || {
            let s = *setup.borrow(); // Copy; borrow released here
            area.queue_draw();
            refresh_status();
            readout.set_text(&format!(
                "Screen ≈ {:.0} × {:.0} mm   ·   horizontal offset {:.0} mm",
                s.width_mm, s.height_mm, s.offset_x_mm
            ));
            let c = s.to_corners();
            corners.set_text(&format!(
                "Corners (mm):  TL({:.0}, {:.0}, {:.0})   TR({:.0}, {:.0}, {:.0})   BL({:.0}, {:.0}, {:.0})",
                c.tl[0], c.tl[1], c.tl[2], c.tr[0], c.tr[1], c.tr[2], c.bl[0], c.bl[1], c.bl[2]
            ));
        })
    };

    // Filled in once the fields exist; see the wiring below the form.
    let on_curve: Hook = Rc::new(RefCell::new(None));

    // Advanced numeric form: compact −[entry]+ spinners.
    let grid = Grid::new();
    grid.set_row_spacing(6);
    grid.set_column_spacing(12);
    grid.set_halign(Align::Center);
    let fields = Rc::new(vec![
        add_spinner(
            &grid,
            0,
            "Width (mm)",
            (
                diagram_width,
                "Measure the visible glass horizontally, from picture edge to \
                 picture edge — do not include the bezel or the case.",
            ),
            5.0,
            1.0,
            5000.0,
            &setup,
            &syncing,
            &refresh_view,
            Rc::new(|s| s.width_mm),
            Rc::new(|s, v| s.width_mm = v),
            None,
        ),
        add_spinner(
            &grid,
            1,
            "Height (mm)",
            (
                diagram_height,
                "Measure the visible glass vertically, from picture edge to \
                 picture edge — do not include the bezel or the case.",
            ),
            5.0,
            1.0,
            5000.0,
            &setup,
            &syncing,
            &refresh_view,
            Rc::new(|s| s.height_mm),
            Rc::new(|s, v| s.height_mm = v),
            None,
        ),
        add_spinner(
            &grid,
            2,
            "Tilt back (deg)",
            (
                diagram_tilt,
                "How far the screen leans back from vertical. 0 is perfectly \
                 upright; most desk monitors sit around 10–20.",
            ),
            1.0,
            -45.0,
            45.0,
            &setup,
            &syncing,
            &refresh_view,
            Rc::new(|s| s.tilt_deg),
            Rc::new(|s, v| s.tilt_deg = v),
            None,
        ),
        add_spinner(
            &grid,
            3,
            "Bottom edge above tracker (mm)",
            (
                diagram_offset_y,
                "Vertical distance from the tracker up to the bottom of the \
                 visible glass. This one matters a lot: an error here shifts \
                 every gaze point vertically.",
            ),
            5.0,
            -2000.0,
            2000.0,
            &setup,
            &syncing,
            &refresh_view,
            Rc::new(|s| s.offset_y_mm),
            Rc::new(|s, v| s.offset_y_mm = v),
            None,
        ),
        add_spinner(
            &grid,
            4,
            "Depth from tracker (mm)",
            (
                diagram_offset_z,
                "How far the bottom of the screen sits behind the tracker — 0 \
                 if they are flush. It mostly affects accuracy near the screen \
                 edges and when you move your head, so it is low-leverage and \
                 can usually stay at its default.",
            ),
            5.0,
            -2000.0,
            2000.0,
            &setup,
            &syncing,
            &refresh_view,
            Rc::new(|s| s.offset_z_mm),
            Rc::new(|s, v| s.offset_z_mm = v),
            None,
        ),
        add_spinner(
            &grid,
            5,
            "Horizontal offset (mm)",
            (
                diagram_offset_x,
                "How far the centre of the screen sits left or right of the \
                 tracker's centre. 0 if the tracker is centred; negative means \
                 the screen sits to the left.",
            ),
            5.0,
            -2000.0,
            2000.0,
            &setup,
            &syncing,
            &refresh_view,
            Rc::new(|s| s.offset_x_mm),
            Rc::new(|s, v| s.offset_x_mm = v),
            None,
        ),
        add_spinner(
            &grid,
            6,
            "Screen curve radius (mm)",
            (
                diagram_curvature,
                "The curve radius from your monitor's spec sheet — \"1800R\" \
                 means 1800. 0 means a flat screen. It matters because the \
                 tracker can only be told about a flat plane, so on a curved \
                 screen the gaze point drifts by a few centimetres through the \
                 middle region of the screen.",
            ),
            100.0,
            0.0,
            5000.0,
            &setup,
            &syncing,
            &refresh_view,
            Rc::new(|s| s.curvature_radius_mm),
            Rc::new(|s, v| s.curvature_radius_mm = v),
            Some(on_curve.clone()),
        ),
    ]);

    // Curvature changed → re-derive the width from the remembered EDID arc.
    // Without this a first-time user of a curved monitor saves the arc width as
    // if it were the chord (22 mm too wide on a 49" 1800R), corrupting both the
    // plane sent to the device and the arc geometry the gaze correction derives
    // from it. Deliberately a no-op when EDID told us nothing: then the width in
    // the form is hand-measured, i.e. already a chord, and must not be rewritten.
    {
        let setup = setup.clone();
        let syncing = syncing.clone();
        let width_entry = fields[0].entry.clone(); // row 0 is "Width (mm)"
        *on_curve.borrow_mut() = Some(Rc::new(move || {
            let Some(arc) = edid_arc_mm else {
                return;
            };
            let r = setup.borrow().curvature_radius_mm; // borrow released here
            let chord = tobii_config::chord_from_arc(arc, r);
            setup.borrow_mut().width_mm = chord;
            syncing.set(true);
            width_entry.set_text(&fmt_val(chord));
            syncing.set(false);
        }));
    }

    let adv_panel = gtk::Box::new(Orientation::Vertical, 10);
    adv_panel.set_halign(Align::Center);
    adv_panel.set_margin_top(6);
    adv_panel.append(&grid);
    adv_panel.append(&corners);
    adv_panel.set_visible(false);

    let refresh_form: Rc<dyn Fn()> = {
        let setup = setup.clone();
        let syncing = syncing.clone();
        let fields = fields.clone();
        Rc::new(move || {
            syncing.set(true);
            let s = *setup.borrow();
            for f in fields.iter() {
                f.entry.set_text(&fmt_val((f.get)(&s)));
            }
            syncing.set(false);
        })
    };

    let header = gtk::Box::new(Orientation::Vertical, 16);
    header.set_halign(Align::Center);
    header.set_valign(Align::Start);
    header.set_margin_top((screen_height() as f64 * 0.34) as i32);
    header.append(&instr);
    header.append(&readout);
    header.append(&status);
    header.append(&buttons);
    header.append(&adv_panel);

    let overlay = Overlay::new();
    overlay.set_child(Some(&area));
    overlay.add_overlay(&header);
    win.set_child(Some(&overlay));

    // Advanced toggle: show/hide the form + reflect its state in the label.
    {
        let adv_panel = adv_panel.clone();
        advanced.connect_toggled(move |t| {
            let on = t.is_active();
            adv_panel.set_visible(on);
            t.set_label(if on { "Hide advanced" } else { "Show advanced" });
        });
    }

    // Drag the nearest line -> update width/offset in the setup.
    let drag = GestureDrag::new();
    let target = Rc::new(RefCell::new(0u8));
    let lines0 = Rc::new(RefCell::new((0.3f64, 0.7f64)));
    {
        let setup = setup.clone();
        let area = area.clone();
        let target = target.clone();
        let lines0 = lines0.clone();
        drag.connect_drag_begin(move |_, x, _| {
            let s = *setup.borrow();
            let (l, r) = align::lines_from_width_offset(s.width_mm, s.offset_x_mm);
            *lines0.borrow_mut() = (l, r);
            let nx = x / area.width().max(1) as f64;
            *target.borrow_mut() = if (nx - l).abs() <= (nx - r).abs() {
                0
            } else {
                1
            };
        });
    }
    {
        let setup = setup.clone();
        let area = area.clone();
        let target = target.clone();
        let lines0 = lines0.clone();
        let refresh_view = refresh_view.clone();
        let refresh_form = refresh_form.clone();
        drag.connect_drag_update(move |_, dx, _| {
            let delta = dx / area.width().max(1) as f64;
            let (l0, r0) = *lines0.borrow();
            let (mut l, mut r) = (l0, r0);
            if *target.borrow() == 0 {
                l = l0 + delta;
            } else {
                r = r0 + delta;
            }
            let (cl, cr) = align::clamp_lines(l, r);
            let a = align::alignment_from_lines(cl, cr, aspect(&area));
            {
                let mut s = setup.borrow_mut();
                s.width_mm = a.width_mm;
                s.height_mm = a.height_mm;
                s.offset_x_mm = a.offset_x_mm;
            }
            refresh_view();
            refresh_form();
        });
    }
    area.add_controller(drag);

    // Hover near a line -> resize cursor.
    let motion = gtk::EventControllerMotion::new();
    {
        let setup = setup.clone();
        let area = area.clone();
        motion.connect_motion(move |_, x, _| {
            let w = area.width().max(1) as f64;
            let nx = x / w;
            let s = *setup.borrow();
            let (l, r) = align::lines_from_width_offset(s.width_mm, s.offset_x_mm);
            let near_px = (nx - l).abs().min((nx - r).abs()) * w;
            area.set_cursor_from_name(Some(if near_px < 14.0 {
                "col-resize"
            } else {
                "default"
            }));
        });
    }
    {
        let area = area.clone();
        motion.connect_leave(move |_| area.set_cursor_from_name(Some("default")));
    }
    area.add_controller(motion);

    // Esc cancels.
    add_escape_to_close(&win);

    // Apply: persist + push to device, return to hub.
    {
        let setup = setup.clone();
        let win = win.clone();
        let status = status.clone();
        apply.connect_clicked(move |_| {
            let s = *setup.borrow();
            let saved = tobii_config::save(&s);
            let _ = cmd_tx.send(DeviceCommand::SetDisplayArea(s.to_corners()));
            // A failed save is not fatal — the geometry is still pushed to the
            // device — but it will not survive a restart, so say so and keep the
            // window open instead of silently closing.
            if let Err(e) = saved {
                status.add_css_class("section-warn");
                status.set_text(&format!(
                    "Applied to the tracker, but saving the configuration failed: {e}. \
                     The settings will be lost when the tracker reconnects."
                ));
                return;
            }
            win.close();
        });
    }
    {
        let win = win.clone();
        cancel.connect_clicked(move |_| win.close());
    }

    // Seed the form + readout from the initial setup.
    refresh_form();
    refresh_view();

    win.present();
}

/// Draw the two draggable vertical alignment lines + a tracker illustration.
fn draw_align(cr: &cairo::Context, w: i32, h: i32, lines: (f64, f64)) {
    let (w, h) = (w as f64, h as f64);

    // Tracker illustration: a bar near the bottom centre.
    let bar_w = w * 0.55;
    let bar_h = 26.0;
    let bx = (w - bar_w) / 2.0;
    let by = h - bar_h - 56.0;

    // Vertical lines run from just above the tracker bar to the screen's bottom.
    let line_top = by - 22.0;
    for x_norm in [lines.0, lines.1] {
        let x = x_norm * w;
        cr.set_source_rgb(0.88, 0.92, 0.96);
        cr.set_line_width(2.0);
        cr.move_to(x, line_top);
        cr.line_to(x, h);
        let _ = cr.stroke();

        // A small upward arrow near the bottom, so it's obvious the lines are
        // the draggable elements. Same normalized x, so it tracks the drag.
        cr.set_source_rgba(0.30, 0.85, 0.85, 0.75);
        cr.move_to(x, h - 34.0);
        cr.line_to(x - 7.0, h - 20.0);
        cr.line_to(x + 7.0, h - 20.0);
        cr.close_path();
        let _ = cr.fill();
    }

    cr.set_source_rgb(0.30, 0.85, 0.85);
    cr.set_line_width(2.0);
    cr.rectangle(bx, by, bar_w, bar_h);
    let _ = cr.stroke();
    cr.arc(w / 2.0, by + bar_h / 2.0, 5.0, 0.0, std::f64::consts::TAU);
    let _ = cr.fill();
}
