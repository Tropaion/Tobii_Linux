//! Preview-my-gaze overlay: a transparent, click-through, always-on-top
//! `gtk4-layer-shell` window that draws a dot at the live 2D gaze point.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gtk::glib;
use gtk::prelude::*;
use gtk::{Application, ApplicationWindow, DrawingArea};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::device::{ConnStatus, DeviceState};
use tobii_config::{correct_gaze_x, DisplaySetup};
use tobii_protocol::gaze::{present, GazeSample};

/// Midpoint of whichever eye origins the sample carries, if any.
fn eye_origin(s: &GazeSample) -> Option<[f64; 3]> {
    let l = s.has(present::EYE_ORIGIN_L).then_some(s.eye_origin_l_mm);
    let r = s.has(present::EYE_ORIGIN_R).then_some(s.eye_origin_r_mm);
    match (l, r) {
        (Some(l), Some(r)) => Some([
            (l[0] + r[0]) / 2.0,
            (l[1] + r[1]) / 2.0,
            (l[2] + r[2]) / 2.0,
        ]),
        (Some(e), None) | (None, Some(e)) => Some(e),
        (None, None) => None,
    }
}

/// Create + present the gaze overlay. Returns the window so the caller can close
/// it later. The per-frame refresh auto-stops when the window is destroyed.
pub fn show(app: &Application, state: Arc<Mutex<DeviceState>>) -> ApplicationWindow {
    let win = ApplicationWindow::builder().application(app).build();
    win.add_css_class("overlay-window");

    // Layer-shell: overlay layer, anchored to all edges (full screen), no focus.
    win.init_layer_shell();
    win.set_layer(Layer::Overlay);
    for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
        win.set_anchor(edge, true);
    }
    win.set_keyboard_mode(KeyboardMode::None);

    // Latest gaze point (normalized), shared with the draw callback.
    let gaze: Rc<Cell<Option<(f64, f64)>>> = Rc::new(Cell::new(None));

    // Saved geometry, for the curved-screen correction below. The device only
    // knows about a flat plane, so on a curved panel its reported x is off by
    // centimetres through the middle of the screen; `correct_gaze_x` maps it
    // back onto the physical arc. A no-op at curvature 0 (every flat setup).
    let setup: Option<DisplaySetup> = tobii_config::load()
        .ok()
        .flatten()
        .filter(|s| s.curvature_radius_mm > 0.0);

    let area = DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    {
        let gaze = gaze.clone();
        area.set_draw_func(move |_, cr, w, h| {
            if let Some((gx, gy)) = gaze.get() {
                let (w, h) = (w as f64, h as f64);
                let x = gx.clamp(0.0, 1.0) * w;
                let y = gy.clamp(0.0, 1.0) * h;
                cr.arc(x, y, 24.0, 0.0, std::f64::consts::TAU);
                cr.set_source_rgba(0.15, 0.85, 0.85, 0.30);
                let _ = cr.fill_preserve();
                cr.set_source_rgba(0.15, 0.85, 0.85, 0.95);
                cr.set_line_width(3.0);
                let _ = cr.stroke();
            }
        });
    }
    win.set_child(Some(&area));

    // Make the whole surface click-through (empty input region).
    win.connect_realize(|w| {
        if let Some(surface) = w.surface() {
            surface.set_input_region(Some(&gtk::cairo::Region::create()));
        }
    });

    // Per-frame refresh; auto-removed when the widget is unrealized (closed).
    {
        let state = state.clone();
        let gaze = gaze.clone();
        area.add_tick_callback(move |area, _| {
            let snap = state.lock().unwrap().clone();
            let g = if matches!(snap.status, ConnStatus::Connected) {
                snap.latest_gaze.as_ref().and_then(|s| {
                    if s.has(present::GAZE_2D) && s.validity_l == 0 {
                        let mut x = s.gaze_point_2d[0];
                        // Curvature is cylindrical about a vertical axis, so
                        // only x needs correcting; y passes through untouched.
                        if let (Some(cfg), Some(eye)) = (setup.as_ref(), eye_origin(s)) {
                            x = correct_gaze_x(x, eye, cfg);
                        }
                        Some((x, s.gaze_point_2d[1]))
                    } else {
                        None
                    }
                })
            } else {
                None
            };
            gaze.set(g);
            area.queue_draw();
            glib::ControlFlow::Continue
        });
    }

    win.present();
    win
}
