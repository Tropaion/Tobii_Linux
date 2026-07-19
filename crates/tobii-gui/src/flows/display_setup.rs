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

    pub fn update(&mut self, ui: &mut egui::Ui, state: &DeviceState) -> SetupOutcome {
        if self.cancelled {
            return SetupOutcome::Cancel;
        }
        let mut outcome = SetupOutcome::Continue;

        ui.vertical_centered(|ui| {
            ui.add_space(20.0);
            ui.heading("Set up your display");
            ui.add_space(12.0);

            match self.step {
                Step::DetectMonitor => {
                    ui.label(
                        "Select your monitor (detected via EDID), or continue with the saved size:",
                    );
                    ui.add_space(8.0);
                    // Snapshot to avoid borrowing self during the click handler.
                    let mons: Vec<(String, f64, f64)> = self
                        .monitors
                        .iter()
                        .filter(|m| m.width_mm > 0.0 && m.height_mm > 0.0)
                        .map(|m| {
                            (
                                format!("{} ({:.0}×{:.0} mm)", m.model, m.width_mm, m.height_mm),
                                m.width_mm,
                                m.height_mm,
                            )
                        })
                        .collect();
                    for (label, w, h) in mons {
                        if ui.button(label).clicked() {
                            self.select_monitor(w, h);
                        }
                    }
                    if ui.button("Use saved size →").clicked() {
                        self.step = Step::Geometry;
                    }
                }
                Step::Geometry => {
                    egui::Grid::new("geometry").num_columns(2).show(ui, |ui| {
                        ui.label("Width (mm)");
                        ui.add(egui::DragValue::new(&mut self.setup.width_mm).range(1.0..=5000.0));
                        ui.end_row();
                        ui.label("Height (mm)");
                        ui.add(
                            egui::DragValue::new(&mut self.setup.height_mm).range(1.0..=5000.0),
                        );
                        ui.end_row();
                        ui.label("Tilt back (deg)");
                        ui.add(egui::DragValue::new(&mut self.setup.tilt_deg).range(-45.0..=45.0));
                        ui.end_row();
                        ui.label("Bottom edge above tracker (mm)");
                        ui.add(egui::DragValue::new(&mut self.setup.offset_y_mm));
                        ui.end_row();
                        ui.label("Depth from tracker (mm)");
                        ui.add(egui::DragValue::new(&mut self.setup.offset_z_mm));
                        ui.end_row();
                        ui.label("Horizontal offset (mm)");
                        ui.add(egui::DragValue::new(&mut self.setup.offset_x_mm));
                        ui.end_row();
                    });
                    let c = self.setup.to_corners();
                    ui.add_space(6.0);
                    ui.label(format!(
                        "Corners (mm): TL({:.0},{:.0},{:.0}) TR({:.0},{:.0},{:.0}) BL({:.0},{:.0},{:.0})",
                        c.tl[0], c.tl[1], c.tl[2], c.tr[0], c.tr[1], c.tr[2], c.bl[0], c.bl[1], c.bl[2]
                    ));
                    ui.add_space(10.0);
                    if ui.button("Apply & continue →").clicked() {
                        outcome = SetupOutcome::Apply(self.confirm_geometry());
                    }
                }
                Step::Confirm => {
                    ui.label("Applied. Confirm the tracker can see you:");
                    let view = state
                        .latest_gaze
                        .as_ref()
                        .map(crate::eyeview::EyeView::from_gaze)
                        .unwrap_or(crate::eyeview::EyeView {
                            left: None,
                            right: None,
                            distance_mm: None,
                            guidance: crate::eyeview::Guidance::NoEyes,
                        });
                    let side = ui.available_width().min(ui.available_height()) * 0.5;
                    crate::widget::draw_eye_view(ui, &view, egui::vec2(side * 1.4, side));
                    ui.label(crate::widget::guidance_message(&view));
                    ui.add_space(12.0);
                    if ui.button("Done").clicked() {
                        outcome = SetupOutcome::Done;
                    }
                }
            }

            ui.add_space(16.0);
            if ui.button("Cancel").clicked() {
                self.cancel();
                outcome = SetupOutcome::Cancel;
            }
        });

        outcome
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

    #[test]
    fn apply_advances_to_confirm_and_yields_setup() {
        let mut f = DisplaySetupFlow::new();
        f.select_monitor(1193.0, 336.0);
        let s = f.confirm_geometry(); // same call the Apply button makes
        assert_eq!((s.width_mm, s.height_mm), (1193.0, 336.0));
        assert!(matches!(f.step(), Step::Confirm));
    }
}
