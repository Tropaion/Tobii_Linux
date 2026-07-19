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
    crate::widget::draw_eye_view(ui, &view, egui::vec2(320.0, 200.0));

    let msg = crate::widget::guidance_message(&view);
    ui.label(msg);
}
