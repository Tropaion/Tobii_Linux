//! Fullscreen display-setup flow (the original's `-S`): drag two vertical lines
//! to the physical ends of the eye tracker; the screen geometry (width + offset)
//! is derived from their positions (`align`), then applied to the device and
//! saved. Cancel/Esc returns to the hub without saving.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use gtk::glib;
use gtk::prelude::*;
use gtk::{cairo, Align, Application, Button, DrawingArea, GestureDrag, Label, Orientation};

use crate::align;
use crate::device::DeviceCommand;
use tobii_config::DisplaySetup;

/// Aspect ratio (w/h) of a widget's current allocation; 1.0 if not yet sized.
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

/// Open the fullscreen display-setup flow window.
pub fn launch(app: &Application, cmd_tx: Sender<DeviceCommand>) {
    let saved = tobii_config::load().ok().flatten();
    let (l0, r0) = match &saved {
        Some(s) => align::lines_from_width_offset(s.width_mm, s.offset_x_mm),
        None => (0.3, 0.7),
    };
    let lines = Rc::new(RefCell::new((l0, r0)));

    let win = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Set up display")
        .build();
    win.set_modal(true);
    win.fullscreen();

    let root = gtk::Box::new(Orientation::Vertical, 12);
    root.set_margin_top(24);
    root.set_margin_bottom(24);
    root.set_margin_start(24);
    root.set_margin_end(24);

    let instr = Label::new(Some(
        "Drag the two lines to the marks on the top corners of your eye tracker.",
    ));
    instr.add_css_class("app-title");
    instr.set_halign(Align::Center);

    let area = DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    {
        let lines = lines.clone();
        area.set_draw_func(move |_, cr, w, h| draw_align(cr, w, h, *lines.borrow()));
    }

    let readout = Label::new(Some("Drag a line to measure your screen…"));
    readout.add_css_class("guidance");
    readout.set_halign(Align::Center);

    let buttons = gtk::Box::new(Orientation::Horizontal, 10);
    buttons.set_halign(Align::Center);
    let apply = Button::with_label("Apply & save");
    let cancel = Button::with_label("Cancel");
    buttons.append(&apply);
    buttons.append(&cancel);

    root.append(&instr);
    root.append(&area);
    root.append(&readout);
    root.append(&buttons);
    win.set_child(Some(&root));

    // Drag the nearest line.
    let drag = GestureDrag::new();
    let target = Rc::new(RefCell::new(0u8)); // 0 = left line, 1 = right line
    let start = Rc::new(RefCell::new(0.0f64)); // the dragged line's position at drag start
    {
        let lines = lines.clone();
        let area = area.clone();
        let target = target.clone();
        let start = start.clone();
        drag.connect_drag_begin(move |_, x, _| {
            let w = area.width().max(1) as f64;
            let nx = x / w;
            let (l, r) = *lines.borrow();
            let t = if (nx - l).abs() <= (nx - r).abs() {
                0
            } else {
                1
            };
            *target.borrow_mut() = t;
            *start.borrow_mut() = if t == 0 { l } else { r };
        });
    }
    {
        let lines = lines.clone();
        let area = area.clone();
        let readout = readout.clone();
        let target = target.clone();
        let start = start.clone();
        drag.connect_drag_update(move |_, dx, _| {
            let w = area.width().max(1) as f64;
            let delta = dx / w;
            let (mut l, mut r) = *lines.borrow();
            let moved = *start.borrow() + delta;
            if *target.borrow() == 0 {
                l = moved;
            } else {
                r = moved;
            }
            let (cl, cr) = align::clamp_lines(l, r);
            *lines.borrow_mut() = (cl, cr);
            area.queue_draw();
            let a = align::alignment_from_lines(cl, cr, aspect(&area));
            readout.set_text(&format!(
                "Screen ≈ {:.0} × {:.0} mm   ·   horizontal offset {:.0} mm",
                a.width_mm, a.height_mm, a.offset_x_mm
            ));
        });
    }
    area.add_controller(drag);

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

    // Apply: derive geometry, persist, push to device, return to hub.
    {
        let lines = lines.clone();
        let area = area.clone();
        let win = win.clone();
        apply.connect_clicked(move |_| {
            let (l, r) = *lines.borrow();
            let a = align::alignment_from_lines(l, r, aspect(&area));
            let base = saved.unwrap_or_else(default_setup);
            let setup = DisplaySetup {
                width_mm: a.width_mm,
                height_mm: a.height_mm,
                offset_x_mm: a.offset_x_mm,
                ..base
            };
            let _ = tobii_config::save(&setup);
            let _ = cmd_tx.send(DeviceCommand::SetDisplayArea(setup.to_corners()));
            win.close();
        });
    }
    {
        let win = win.clone();
        cancel.connect_clicked(move |_| win.close());
    }

    win.present();
}

/// Draw the two draggable vertical alignment lines + a tracker illustration.
fn draw_align(cr: &cairo::Context, w: i32, h: i32, lines: (f64, f64)) {
    let (w, h) = (w as f64, h as f64);

    // Two full-height vertical lines with drag-handle arrows near the bottom.
    cr.set_source_rgb(0.88, 0.92, 0.96);
    cr.set_line_width(2.0);
    for x_norm in [lines.0, lines.1] {
        let x = x_norm * w;
        cr.move_to(x, 0.0);
        cr.line_to(x, h);
        let _ = cr.stroke();
    }

    // Tracker illustration: a rounded bar near the bottom centre.
    let bar_w = w * 0.55;
    let bar_h = 26.0;
    let bx = (w - bar_w) / 2.0;
    let by = h - bar_h - 48.0;
    cr.set_source_rgb(0.30, 0.85, 0.85);
    cr.set_line_width(2.0);
    cr.rectangle(bx, by, bar_w, bar_h);
    let _ = cr.stroke();
    // Centre lens dot.
    cr.arc(w / 2.0, by + bar_h / 2.0, 5.0, 0.0, std::f64::consts::TAU);
    let _ = cr.fill();
}
