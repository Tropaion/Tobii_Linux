//! Guided display-setup flow: fullscreen corner-calibration wizard.
//! Stub this task — the real body lands in a later task; this satisfies
//! routing in `lib.rs`.

use crate::device::DeviceState;
use eframe::egui;

pub enum SetupOutcome {
    Continue,
    Apply(tobii_config::DisplaySetup),
    Done,
    Cancel,
}

pub struct DisplaySetupFlow;

impl Default for DisplaySetupFlow {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplaySetupFlow {
    pub fn new() -> Self {
        Self
    }

    pub fn update(&mut self, ui: &mut egui::Ui, _state: &DeviceState) -> SetupOutcome {
        if ui.button("Cancel").clicked() {
            SetupOutcome::Cancel
        } else {
            SetupOutcome::Continue
        }
    }
}
