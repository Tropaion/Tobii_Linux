//! `tobii-gui` — graphical configuration hub for the Tobii ET5.
//!
//! A persistent hub window (status + live eye position + flow launchers) over a
//! device thread that owns the blocking USB connection. (Flows are added in B2.2.)
//! `main.rs` is a thin entry point; the app + modules live here so their `pub`
//! items are library API rather than dead code in a binary.

pub mod device;
pub mod eyeview;
pub mod flows;
pub mod hub;
pub mod widget;

use crate::flows::display_setup::{DisplaySetupFlow, SetupOutcome};
use crate::flows::eye_position::{EyeFlowOutcome, EyePositionFlow};
use crate::hub::HubAction;
use eframe::egui;

/// Run the app: open the window, start the device thread, render the hub.
pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([720.0, 480.0])
            .with_title("Tobii Configuration"),
        ..Default::default()
    };
    eframe::run_native(
        "tobii-gui",
        options,
        Box::new(|_cc| Ok(Box::new(TobiiApp::new()))),
    )
}

/// Which screen is currently shown. The hub is windowed; guided flows take
/// over fullscreen (toggled in `update` on entry/exit).
enum Screen {
    Hub,
    DisplaySetup(DisplaySetupFlow),
    EyePosition(EyePositionFlow),
}

struct TobiiApp {
    state: std::sync::Arc<std::sync::Mutex<device::DeviceState>>,
    cmd_tx: std::sync::mpsc::Sender<device::DeviceCommand>,
    screen: Screen,
}

impl TobiiApp {
    fn new() -> Self {
        let (state, cmd_tx) = device::spawn();
        Self {
            state,
            cmd_tx,
            screen: Screen::Hub,
        }
    }
}

impl eframe::App for TobiiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let snapshot = self.state.lock().unwrap().clone();
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));

        egui::CentralPanel::default().show(ctx, |ui| match &mut self.screen {
            Screen::Hub => {
                if let Some(action) = hub::draw(ui, &snapshot) {
                    self.screen = match action {
                        HubAction::SetupDisplay => Screen::DisplaySetup(DisplaySetupFlow::new()),
                        HubAction::PositionEyes => Screen::EyePosition(EyePositionFlow::new()),
                    };
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                }
            }
            Screen::EyePosition(flow) => {
                let out = flow.update(ui, &snapshot);
                if esc || matches!(out, EyeFlowOutcome::Done) {
                    self.screen = Screen::Hub;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }
            }
            Screen::DisplaySetup(flow) => {
                let out = flow.update(ui, &snapshot);
                if let SetupOutcome::Apply(setup) = &out {
                    let _ = tobii_config::save(setup);
                    let _ = self
                        .cmd_tx
                        .send(device::DeviceCommand::SetDisplayArea(setup.to_corners()));
                }
                if esc || matches!(out, SetupOutcome::Done | SetupOutcome::Cancel) {
                    self.screen = Screen::Hub;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }
            }
        });

        // Esc while already in the hub does nothing; handled per-screen above.
        ctx.request_repaint_after(std::time::Duration::from_millis(33)); // ~30fps live view
    }
}
