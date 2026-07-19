//! `tobii-gui` — graphical configuration hub for the Tobii ET5.
//!
//! A persistent hub window (status + live eye position + flow launchers) over a
//! device thread that owns the blocking USB connection. (Flows are added in B2.2.)
//! `main.rs` is a thin entry point; the app + modules live here so their `pub`
//! items are library API rather than dead code in a binary.

pub mod device;
pub mod eyeview;
pub mod hub;

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

struct TobiiApp {
    state: std::sync::Arc<std::sync::Mutex<device::DeviceState>>,
    _cmd_tx: std::sync::mpsc::Sender<device::DeviceCommand>,
}

impl TobiiApp {
    fn new() -> Self {
        let (state, cmd_tx) = device::spawn();
        Self {
            state,
            _cmd_tx: cmd_tx,
        }
    }
}

impl eframe::App for TobiiApp {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        let snapshot = self.state.lock().unwrap().clone();
        eframe::egui::CentralPanel::default().show(ctx, |ui| hub::draw(ui, &snapshot));
        ctx.request_repaint_after(std::time::Duration::from_millis(33)); // ~30fps live view
    }
}
