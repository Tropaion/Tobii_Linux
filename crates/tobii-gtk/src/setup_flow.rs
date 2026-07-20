//! Fullscreen display-setup flow (the original's `-S`): drag two vertical lines
//! to the physical ends of the eye tracker; the screen geometry (width + offset)
//! is derived from their positions (`align`). An "Advanced" toggle reveals the
//! editable numeric form (two-way synced with the drag). Apply persists +
//! pushes to the device; Cancel/Esc returns to the hub without saving.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::Sender;

use gtk::glib;
use gtk::prelude::*;
use gtk::{
    cairo, Align, Application, Button, DrawingArea, GestureDrag, Grid, Label, Orientation, Overlay,
    SpinButton, ToggleButton,
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

/// Wire a spin button so edits write `set(setup, value)` and refresh the view,
/// unless we are the ones programmatically setting it (`syncing`).
fn wire_spin(
    sb: &SpinButton,
    setup: Rc<RefCell<DisplaySetup>>,
    syncing: Rc<Cell<bool>>,
    refresh_view: Rc<dyn Fn()>,
    set: impl Fn(&mut DisplaySetup, f64) + 'static,
) {
    sb.connect_value_changed(move |sb| {
        if syncing.get() {
            return;
        }
        set(&mut setup.borrow_mut(), sb.value());
        refresh_view();
    });
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

    // Header widgets (overlaid, centred, nudged a bit above the middle).
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

    // Advanced numeric form (hidden until the toggle is on).
    let sb_w = SpinButton::with_range(1.0, 5000.0, 1.0);
    let sb_h = SpinButton::with_range(1.0, 5000.0, 1.0);
    let sb_tilt = SpinButton::with_range(-45.0, 45.0, 1.0);
    let sb_ox = SpinButton::with_range(-2000.0, 2000.0, 1.0);
    let sb_oy = SpinButton::with_range(-2000.0, 2000.0, 1.0);
    let sb_oz = SpinButton::with_range(-2000.0, 2000.0, 1.0);
    let corners = Label::new(Some(""));
    corners.add_css_class("section-desc");

    let grid = Grid::new();
    grid.set_row_spacing(6);
    grid.set_column_spacing(10);
    grid.set_halign(Align::Center);
    let rows: [(&str, &SpinButton); 6] = [
        ("Width (mm)", &sb_w),
        ("Height (mm)", &sb_h),
        ("Tilt back (deg)", &sb_tilt),
        ("Bottom edge above tracker (mm)", &sb_oy),
        ("Depth from tracker (mm)", &sb_oz),
        ("Horizontal offset (mm)", &sb_ox),
    ];
    for (i, (label, sb)) in rows.iter().enumerate() {
        let l = Label::new(Some(label));
        l.set_halign(Align::Start);
        grid.attach(&l, 0, i as i32, 1, 1);
        grid.attach(*sb, 1, i as i32, 1, 1);
    }
    let adv_panel = gtk::Box::new(Orientation::Vertical, 10);
    adv_panel.set_halign(Align::Center);
    adv_panel.append(&grid);
    adv_panel.append(&corners);
    adv_panel.set_visible(false);

    let header = gtk::Box::new(Orientation::Vertical, 16);
    header.set_halign(Align::Center);
    header.set_valign(Align::Center);
    header.set_margin_bottom(140); // bias the block a bit above the middle
    header.append(&instr);
    header.append(&readout);
    header.append(&buttons);
    header.append(&adv_panel);

    let overlay = Overlay::new();
    overlay.set_child(Some(&area));
    overlay.add_overlay(&header);
    win.set_child(Some(&overlay));

    // --- shared refresh closures ---
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

    let refresh_form: Rc<dyn Fn()> = {
        let setup = setup.clone();
        let syncing = syncing.clone();
        let (sb_w, sb_h, sb_tilt, sb_ox, sb_oy, sb_oz) = (
            sb_w.clone(),
            sb_h.clone(),
            sb_tilt.clone(),
            sb_ox.clone(),
            sb_oy.clone(),
            sb_oz.clone(),
        );
        Rc::new(move || {
            syncing.set(true);
            let s = *setup.borrow();
            sb_w.set_value(s.width_mm);
            sb_h.set_value(s.height_mm);
            sb_tilt.set_value(s.tilt_deg);
            sb_ox.set_value(s.offset_x_mm);
            sb_oy.set_value(s.offset_y_mm);
            sb_oz.set_value(s.offset_z_mm);
            syncing.set(false);
        })
    };

    // Advanced form edits -> setup.
    wire_spin(
        &sb_w,
        setup.clone(),
        syncing.clone(),
        refresh_view.clone(),
        |s, v| s.width_mm = v,
    );
    wire_spin(
        &sb_h,
        setup.clone(),
        syncing.clone(),
        refresh_view.clone(),
        |s, v| s.height_mm = v,
    );
    wire_spin(
        &sb_tilt,
        setup.clone(),
        syncing.clone(),
        refresh_view.clone(),
        |s, v| s.tilt_deg = v,
    );
    wire_spin(
        &sb_ox,
        setup.clone(),
        syncing.clone(),
        refresh_view.clone(),
        |s, v| s.offset_x_mm = v,
    );
    wire_spin(
        &sb_oy,
        setup.clone(),
        syncing.clone(),
        refresh_view.clone(),
        |s, v| s.offset_y_mm = v,
    );
    wire_spin(
        &sb_oz,
        setup.clone(),
        syncing.clone(),
        refresh_view.clone(),
        |s, v| s.offset_z_mm = v,
    );

    // Advanced toggle shows/hides the form.
    {
        let adv_panel = adv_panel.clone();
        advanced.connect_toggled(move |t| adv_panel.set_visible(t.is_active()));
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
