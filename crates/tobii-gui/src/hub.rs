//! The persistent hub screen: status, flow launchers (stubs in the foundation),
//! and a live trackbox mini-view.

use eframe::egui;

use crate::device::{ConnStatus, DeviceState};
use crate::eyeview::{EyeView, Guidance};

pub fn draw(ui: &mut egui::Ui, state: &DeviceState) {
    ui.heading("Tobii Configuration");
    ui.add_space(6.0);

    match &state.status {
        ConnStatus::Connecting => {
            ui.colored_label(egui::Color32::YELLOW, "Connecting to the eye tracker…");
        }
        ConnStatus::Connected => {
            ui.colored_label(egui::Color32::GREEN, "Eye tracker connected");
        }
        ConnStatus::Error(e) => {
            ui.colored_label(egui::Color32::LIGHT_RED, format!("Not connected: {e}"));
        }
    }
    ui.add_space(10.0);

    ui.horizontal(|ui| {
        // Flows arrive in B2.2 — buttons are placeholders here.
        let _ = ui.button("Set up display…");
        let _ = ui.button("Position eyes…");
        ui.add_enabled(false, egui::Button::new("Calibrate… (B3)"));
    });
    ui.add_space(12.0);

    ui.label("Eye position:");
    let view = state
        .latest_gaze
        .as_ref()
        .map(EyeView::from_gaze)
        .unwrap_or(EyeView {
            left: None,
            right: None,
            distance_mm: None,
            guidance: Guidance::NoEyes,
        });
    draw_trackbox(ui, &view, egui::vec2(320.0, 200.0));

    let msg = match view.guidance {
        Guidance::NoEyes => "No eyes detected — sit in front of the tracker.".to_string(),
        Guidance::MoveCloser => "Move a little closer.".to_string(),
        Guidance::MoveBack => "Move back a little.".to_string(),
        Guidance::OffCenter => "Center yourself in front of the screen.".to_string(),
        Guidance::Centered => match view.distance_mm {
            Some(d) => format!("Good position ({d:.0} mm)."),
            None => "Good position.".to_string(),
        },
    };
    ui.label(msg);
}

/// Draw the trackbox rectangle with the two eyes at their normalized positions.
fn draw_trackbox(ui: &mut egui::Ui, view: &EyeView, size: egui::Vec2) {
    let (resp, painter) = ui.allocate_painter(size, egui::Sense::hover());
    let rect = resp.rect;
    painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.5, egui::Color32::GRAY));
    let plot = |p: [f32; 2]| {
        egui::pos2(
            rect.left() + p[0].clamp(0.0, 1.0) * rect.width(),
            rect.top() + p[1].clamp(0.0, 1.0) * rect.height(),
        )
    };
    let color = if matches!(view.guidance, Guidance::Centered) {
        egui::Color32::GREEN
    } else {
        egui::Color32::YELLOW
    };
    for eye in [view.left, view.right].into_iter().flatten() {
        painter.circle_filled(plot(eye), 8.0, color);
    }
}
