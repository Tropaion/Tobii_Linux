//! The fullscreen display-setup flow (the original's `-S`): forward-only
//! DetectMonitor -> Geometry -> Confirm, then Done (return to hub). Cancel at
//! any step returns to the hub without saving. Reuses Plan 4's `DisplaySetup`
//! math + `tobii-config` EDID/TOML.

use eframe::egui;

use crate::device::DeviceState;
use tobii_config::{detect_monitors, DisplaySetup, MonitorInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    DetectMonitor,
    Geometry,
    Confirm,
}

pub enum SetupOutcome {
    Continue,
    Apply(DisplaySetup),
    Done,
    Cancel,
}

pub struct DisplaySetupFlow {
    step: Step,
    setup: DisplaySetup,
    monitors: Vec<MonitorInfo>,
    cancelled: bool,
}

/// Seed geometry: prefer the saved config, else defaults; dims are refined by
/// monitor selection in the DetectMonitor step.
fn seed_setup() -> DisplaySetup {
    tobii_config::load().ok().flatten().unwrap_or(DisplaySetup {
        width_mm: 600.0,
        height_mm: 340.0,
        tilt_deg: 20.0,
        offset_x_mm: 0.0,
        offset_y_mm: 10.0,
        offset_z_mm: 0.0,
    })
}

impl Default for DisplaySetupFlow {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplaySetupFlow {
    pub fn new() -> Self {
        Self {
            step: Step::DetectMonitor,
            setup: seed_setup(),
            monitors: detect_monitors(),
            cancelled: false,
        }
    }

    pub fn step(&self) -> &Step {
        &self.step
    }
    pub fn setup(&self) -> &DisplaySetup {
        &self.setup
    }
    pub fn monitors(&self) -> &[MonitorInfo] {
        &self.monitors
    }
    pub fn cancelled(&self) -> bool {
        self.cancelled
    }

    pub fn select_monitor(&mut self, width_mm: f64, height_mm: f64) {
        self.setup.width_mm = width_mm;
        self.setup.height_mm = height_mm;
        self.step = Step::Geometry;
    }

    /// Leaves the geometry form for the confirmation step and returns the setup
    /// the caller must persist + apply to the device.
    pub fn confirm_geometry(&mut self) -> DisplaySetup {
        self.step = Step::Confirm;
        self.setup
    }

    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    pub fn update(&mut self, _ui: &mut egui::Ui, _state: &DeviceState) -> SetupOutcome {
        // Rendering + interaction land in Task 5. Until then, no-op.
        SetupOutcome::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_detect_monitor_with_defaults() {
        let f = DisplaySetupFlow::new();
        assert!(matches!(f.step(), Step::DetectMonitor));
        assert!(f.setup().width_mm > 0.0 && f.setup().height_mm > 0.0);
    }

    #[test]
    fn select_monitor_sets_dims_and_advances() {
        let mut f = DisplaySetupFlow::new();
        f.select_monitor(1193.0, 336.0);
        assert!(matches!(f.step(), Step::Geometry));
        assert_eq!(f.setup().width_mm, 1193.0);
        assert_eq!(f.setup().height_mm, 336.0);
    }

    #[test]
    fn confirm_geometry_returns_setup_and_advances_to_confirm() {
        let mut f = DisplaySetupFlow::new();
        f.select_monitor(600.0, 340.0);
        let s = f.confirm_geometry();
        assert_eq!(s.width_mm, 600.0);
        assert!(matches!(f.step(), Step::Confirm));
    }

    #[test]
    fn cancel_is_observable() {
        let mut f = DisplaySetupFlow::new();
        f.cancel();
        assert!(f.cancelled());
    }
}
