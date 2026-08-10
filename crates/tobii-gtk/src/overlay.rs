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
use tobii_protocol::gaze::present;

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
    // Cover the WHOLE output, panels included. Anchoring to four edges is not
    // enough: a layer surface is shrunk to whatever other surfaces' exclusive
    // zones leave behind, so a desktop panel silently steals a strip. The gaze
    // point is normalized against the full screen, so a surface that is short
    // by a 44px taskbar draws every dot about 3% high — around 10mm up at the
    // bottom of a 336mm-tall screen, uniformly, forever. -1 opts out.
    win.set_exclusive_zone(-1);
    win.set_keyboard_mode(KeyboardMode::None);

    // Latest gaze point (normalized), shared with the draw callback.
    let gaze: Rc<Cell<Option<(f64, f64)>>> = Rc::new(Cell::new(None));

    let area = DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    {
        let gaze = gaze.clone();
        let checked = Cell::new(false);
        area.set_draw_func(move |_, cr, w, h| {
            // Gaze is normalized against the whole screen, so this surface has
            // to BE the whole screen. If a compositor hands us less, every dot
            // is offset and scaled by exactly that difference — an error which
            // looks identical to the tracker being miscalibrated, and which no
            // amount of recalibration fixes. Say so once rather than let it
            // masquerade.
            if !checked.replace(true) {
                if let Some(geo) = crate::primary_monitor().map(|m| m.geometry()) {
                    if (geo.width() - w).abs() > 1 || (geo.height() - h).abs() > 1 {
                        eprintln!(
                            "gaze overlay: drawing into {w}x{h} but the monitor is {}x{} — \
                             every dot will be off by up to ({}, {}) px. Something is \
                             reserving screen space (a panel/taskbar).",
                            geo.width(),
                            geo.height(),
                            geo.width() - w,
                            geo.height() - h,
                        );
                    }
                }
            }
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
                    // Either eye valid is enough. Gating on the LEFT eye alone
                    // (as this did) makes the overlay work in left-eye-only
                    // mode and silently show nothing in right-eye-only mode.
                    // The combined gaze point is device-computed from whichever
                    // eyes it has, so one valid eye is a usable reading.
                    if s.has(present::GAZE_2D) && (s.validity_l == 0 || s.validity_r == 0) {
                        // NO curvature correction here. A per-user
                        // calibration already absorbs screen curvature: the
                        // stimulus dots are drawn on the physical (curved)
                        // screen but reported to the device as plain normalized
                        // coordinates, so the device learns to emit those
                        // coordinates for those physical points. Post-correcting
                        // double-counts it — zero error at centre, growing to
                        // centimetres at the sides. Verified on hardware
                        // 2026-07-21; `tobiifree` likewise applies no such
                        // correction.
                        Some((s.gaze_point_2d[0], s.gaze_point_2d[1]))
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
