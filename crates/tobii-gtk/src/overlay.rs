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

/// Midpoint of the eye origins that are actually **tracked**, if any.
///
/// The present bit alone is not enough: the device populates both eye-origin
/// columns with all-zero points even in frames where it sees no eyes at all
/// (see the captured real-device frame in `tobii_protocol::gaze`'s tests). An
/// untracked eye averaged in as `[0, 0, 0]` halves the midpoint's distance and
/// lateral offset, which roughly doubles the curvature correction and flips its
/// direction. So gate each eye on its validity too, exactly as `eyeview` does.
fn eye_origin(s: &GazeSample) -> Option<[f64; 3]> {
    let l = (s.has(present::EYE_ORIGIN_L) && s.validity_l == 0).then_some(s.eye_origin_l_mm);
    let r = (s.has(present::EYE_ORIGIN_R) && s.validity_r == 0).then_some(s.eye_origin_r_mm);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The user's real monitor: 49" 1800R, 1171 mm chord.
    fn odyssey() -> DisplaySetup {
        DisplaySetup {
            width_mm: 1171.0,
            height_mm: 336.0,
            tilt_deg: 0.0,
            offset_x_mm: 0.0,
            offset_y_mm: 100.0,
            offset_z_mm: 0.0,
            curvature_radius_mm: 1800.0,
        }
    }

    fn sample(l: [f64; 3], r: [f64; 3], validity_l: u32, validity_r: u32) -> GazeSample {
        GazeSample {
            present_mask: present::EYE_ORIGIN_L | present::EYE_ORIGIN_R,
            validity_l,
            validity_r,
            eye_origin_l_mm: l,
            eye_origin_r_mm: r,
            ..GazeSample::default()
        }
    }

    #[test]
    fn both_eyes_tracked_gives_the_midpoint() {
        let s = sample([-30.0, 400.0, -700.0], [30.0, 400.0, -700.0], 0, 0);
        assert_eq!(eye_origin(&s), Some([0.0, 400.0, -700.0]));
    }

    /// The device sets the eye-origin present bits *and* zeroes the columns for
    /// an eye it is not tracking, so a present-bit-only test averages a phantom
    /// eye at the tracker origin into the midpoint.
    #[test]
    fn an_untracked_eye_is_not_averaged_in() {
        let tracked = [-30.0, 400.0, -700.0];
        let s = sample(tracked, [0.0; 3], 0, 4);
        assert_eq!(eye_origin(&s), Some(tracked));
        // Mirror case: the other eye is the tracked one.
        let s = sample([0.0; 3], tracked, 4, 0);
        assert_eq!(eye_origin(&s), Some(tracked));
    }

    #[test]
    fn no_tracked_eye_means_no_correction() {
        let s = sample([0.0; 3], [0.0; 3], 4, 4);
        assert_eq!(eye_origin(&s), None);
    }

    /// The consequence, in screen millimetres: averaging in the zeroed origin
    /// halves the eye's distance and lateral offset, which roughly doubles the
    /// correction *and* points it the wrong way.
    #[test]
    fn averaging_a_zeroed_origin_would_double_the_correction() {
        let cfg = odyssey();
        let tracked = [-30.0, 400.0, -700.0];
        let s = sample(tracked, [0.0; 3], 0, 4);
        let good = correct_gaze_x(0.25, eye_origin(&s).expect("one tracked eye"), &cfg);
        assert!((good - 0.231281).abs() < 1e-6, "good={good}");
        // What the present-bit-only version produced: the halved midpoint.
        let halved = correct_gaze_x(0.25, [-15.0, 200.0, -350.0], &cfg);
        assert!((halved - 0.209718).abs() < 1e-6, "halved={halved}");
        // ~25 mm apart on a 1171 mm screen — the size of the bug this fixes.
        assert!((good - halved).abs() * 1171.0 > 20.0);
    }
}
