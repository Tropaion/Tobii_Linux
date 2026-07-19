//! `tobii-gtk` — GTK4 configuration GUI for the Tobii ET5 Linux runtime.
//!
//! Scaffold: a minimal `gtk::Application` window, to confirm the GTK4 toolchain.
//! The hub, guided flows, gaze overlay, and the ported device thread land in the
//! GTK4 redesign plan.

use gtk::prelude::*;
use gtk::{Application, ApplicationWindow};

const APP_ID: &str = "com.tobiilinux.Configuration";

/// Run the GTK application.
pub fn run() -> gtk::glib::ExitCode {
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &Application) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Tobii Configuration")
        .default_width(720)
        .default_height(480)
        .build();
    window.present();
}
