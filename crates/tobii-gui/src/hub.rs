//! The persistent hub screen: status, flow launchers, and a live trackbox
//! mini-view. `draw` reports which launcher (if any) was clicked this frame;
//! `lib.rs` owns the resulting screen transition.

use eframe::egui;

use crate::device::{ConnStatus, DeviceState};

/// A launcher the user activated in the hub this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubAction {
    SetupDisplay,
    PositionEyes,
}

pub fn draw(ui: &mut egui::Ui, state: &DeviceState) -> Option<HubAction> {
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

    let mut action = None;
    ui.horizontal(|ui| {
        if ui.button("Set up display…").clicked() {
            action = Some(HubAction::SetupDisplay);
        }
        if ui.button("Position eyes…").clicked() {
            action = Some(HubAction::PositionEyes);
        }
        ui.add_enabled(false, egui::Button::new("Calibrate… (B3)"));
    });
    ui.add_space(12.0);

    ui.label("Eye position:");
    let view = crate::widget::eye_view_for(state);
    crate::widget::draw_eye_view(ui, &view, egui::vec2(320.0, 200.0));

    let msg = crate::widget::guidance_message(&view);
    ui.label(msg);

    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_action_variants_exist() {
        // Compile-level guarantee the launcher actions are modeled.
        let a = HubAction::SetupDisplay;
        let b = HubAction::PositionEyes;
        assert_ne!(std::mem::discriminant(&a), std::mem::discriminant(&b));
    }
}
