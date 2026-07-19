//! Guided eye-position flow: fullscreen trackbox view with live guidance.
//! Stub this task — the real body (rendering + Escape/Done handling context)
//! lands in a later task; this satisfies routing in `lib.rs`.

use crate::device::DeviceState;
use eframe::egui;

pub struct EyePositionFlow;

pub enum EyeFlowOutcome {
    Continue,
    Done,
}

impl Default for EyePositionFlow {
    fn default() -> Self {
        Self::new()
    }
}

impl EyePositionFlow {
    pub fn new() -> Self {
        Self
    }

    pub fn update(&mut self, ui: &mut egui::Ui, _state: &DeviceState) -> EyeFlowOutcome {
        if ui.button("Done").clicked() {
            EyeFlowOutcome::Done
        } else {
            EyeFlowOutcome::Continue
        }
    }
}
