//! The fullscreen "position your eyes" flow (the original's `-P`): the shared
//! eye-position view at large size + guidance, closable with Done or Esc.

use eframe::egui;

use crate::device::{ConnStatus, DeviceState};
use crate::widget::{disconnect_status_line, draw_eye_view, eye_view_for, guidance_message};

#[derive(Default)]
pub struct EyePositionFlow;

pub enum EyeFlowOutcome {
    Continue,
    Done,
}

impl EyePositionFlow {
    pub fn new() -> Self {
        Self
    }

    /// Pure finish-decision so the transition is unit-testable without egui.
    pub fn outcome_for(finish: bool) -> EyeFlowOutcome {
        if finish {
            EyeFlowOutcome::Done
        } else {
            EyeFlowOutcome::Continue
        }
    }

    pub fn update(&mut self, ui: &mut egui::Ui, state: &DeviceState) -> EyeFlowOutcome {
        let view = eye_view_for(state);

        let mut finish = false;
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.heading("Position your eyes");
            ui.add_space(16.0);
            if !matches!(state.status, ConnStatus::Connected) {
                disconnect_status_line(ui, &state.status);
                ui.add_space(8.0);
            }
            let side = ui.available_width().min(ui.available_height()) * 0.6;
            draw_eye_view(ui, &view, egui::vec2(side * 1.4, side));
            ui.add_space(16.0);
            ui.label(guidance_message(&view));
            ui.add_space(24.0);
            if ui.button("Done").clicked() {
                finish = true;
            }
            ui.label("(press Esc to return)");
        });
        Self::outcome_for(finish)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_requested_yields_done() {
        assert!(matches!(
            EyePositionFlow::outcome_for(true),
            EyeFlowOutcome::Done
        ));
        assert!(matches!(
            EyePositionFlow::outcome_for(false),
            EyeFlowOutcome::Continue
        ));
    }
}
