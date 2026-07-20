//! Fullscreen display-setup flow (the original's `-S`): drag two vertical lines
//! to the physical ends of the eye tracker; the screen geometry (width + offset)
//! is derived from their positions (`align`). A "Show advanced" toggle reveals
//! the editable numeric form (compact −[value]+ spinners, two-way synced with
//! the drag). Apply persists + pushes to the device; Cancel/Esc returns.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::Sender;

use gtk::glib;
use gtk::prelude::*;
use gtk::{
    cairo, Align, Application, Button, DrawingArea, Entry, GestureDrag, Grid, Label, Orientation,
    Overlay, ToggleButton,
};

use crate::align;
use crate::device::DeviceCommand;
use tobii_config::DisplaySetup;

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
    }
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

fn fmt_val(v: f64) -> String {
    format!("{v:.0}")
}

type Getter = Rc<dyn Fn(&DisplaySetup) -> f64>;
type Setter = Rc<dyn Fn(&mut DisplaySetup, f64)>;

/// One editable field: its entry + a getter for refreshing it from the setup.
struct Field {
    entry: Entry,
    get: Getter,
}

/// Build + wire one compact `−[entry]+` spinner row into the grid.
#[allow(clippy::too_many_arguments)]
fn add_spinner(
    grid: &Grid,
    row: i32,
    label: &str,
    step: f64,
    min: f64,
    max: f64,
    setup: &Rc<RefCell<DisplaySetup>>,
    syncing: &Rc<Cell<bool>>,
    refresh_view: &Rc<dyn Fn()>,
    get: Getter,
    set: Setter,
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

    // Commit a value: clamp, store, reflect in the entry (guarded), refresh view.
    let commit: Rc<dyn Fn(f64)> = {
        let setup = setup.clone();
        let syncing = syncing.clone();
        let refresh_view = refresh_view.clone();
        let entry = entry.clone();
        let set = set.clone();
        Rc::new(move |v: f64| {
            let v = v.clamp(min, max);
            set(&mut setup.borrow_mut(), v);
            syncing.set(true);
            entry.set_text(&fmt_val(v));
            syncing.set(false);
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
                set(&mut setup.borrow_mut(), v.clamp(min, max));
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
    let saved = tobii_config::load().ok().flatten();
    let setup = Rc::new(RefCell::new(saved.unwrap_or_else(default_setup)));

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
        Rc::new(move || {
            let s = *setup.borrow();
            area.queue_draw();
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
            5.0,
            1.0,
            5000.0,
            &setup,
            &syncing,
            &refresh_view,
            Rc::new(|s| s.width_mm),
            Rc::new(|s, v| s.width_mm = v),
        ),
        add_spinner(
            &grid,
            1,
            "Height (mm)",
            5.0,
            1.0,
            5000.0,
            &setup,
            &syncing,
            &refresh_view,
            Rc::new(|s| s.height_mm),
            Rc::new(|s, v| s.height_mm = v),
        ),
        add_spinner(
            &grid,
            2,
            "Tilt back (deg)",
            1.0,
            -45.0,
            45.0,
            &setup,
            &syncing,
            &refresh_view,
            Rc::new(|s| s.tilt_deg),
            Rc::new(|s, v| s.tilt_deg = v),
        ),
        add_spinner(
            &grid,
            3,
            "Bottom edge above tracker (mm)",
            5.0,
            -2000.0,
            2000.0,
            &setup,
            &syncing,
            &refresh_view,
            Rc::new(|s| s.offset_y_mm),
            Rc::new(|s, v| s.offset_y_mm = v),
        ),
        add_spinner(
            &grid,
            4,
            "Depth from tracker (mm)",
            5.0,
            -2000.0,
            2000.0,
            &setup,
            &syncing,
            &refresh_view,
            Rc::new(|s| s.offset_z_mm),
            Rc::new(|s, v| s.offset_z_mm = v),
        ),
        add_spinner(
            &grid,
            5,
            "Horizontal offset (mm)",
            5.0,
            -2000.0,
            2000.0,
            &setup,
            &syncing,
            &refresh_view,
            Rc::new(|s| s.offset_x_mm),
            Rc::new(|s, v| s.offset_x_mm = v),
        ),
    ]);

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

    // Apply: persist + push to device, return to hub.
    {
        let setup = setup.clone();
        let win = win.clone();
        apply.connect_clicked(move |_| {
            let s = *setup.borrow();
            let _ = tobii_config::save(&s);
            let _ = cmd_tx.send(DeviceCommand::SetDisplayArea(s.to_corners()));
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
    cr.set_source_rgb(0.88, 0.92, 0.96);
    cr.set_line_width(2.0);
    for x_norm in [lines.0, lines.1] {
        let x = x_norm * w;
        cr.move_to(x, line_top);
        cr.line_to(x, h);
        let _ = cr.stroke();
    }

    cr.set_source_rgb(0.30, 0.85, 0.85);
    cr.set_line_width(2.0);
    cr.rectangle(bx, by, bar_w, bar_h);
    let _ = cr.stroke();
    cr.arc(w / 2.0, by + bar_h / 2.0, 5.0, 0.0, std::f64::consts::TAU);
    let _ = cr.fill();
}
