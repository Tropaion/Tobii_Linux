//! `tobii-gui` — graphical configuration hub for the Tobii ET5.
//!
//! A persistent hub window (status + live eye position + flow launchers) over a
//! device thread that owns the blocking USB connection. (Flows are added in B2.2.)
//! `main.rs` is a thin entry point; the app + modules live here so their `pub`
//! items are library API rather than dead code in a binary.

pub mod device;
pub mod eyeview;

/// Run the app: open the window and start the egui event loop.
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
        Box::new(|_cc| Ok(Box::<TobiiApp>::default())),
    )
}

#[derive(Default)]
struct TobiiApp {}

impl eframe::App for TobiiApp {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        eframe::egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Tobii Configuration");
            ui.label("(foundation — hub UI arrives in a later task)");
        });
    }
}
