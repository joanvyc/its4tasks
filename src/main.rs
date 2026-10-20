//! The its4tasks GUI. It shows the board kept by `its4tasks-server`, at
//! `$ITS4TASKS_SERVER` (by default http://127.0.0.1:7878).

mod app;

use its4tasks::{api, client, log, model};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("its4tasks")
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([700.0, 450.0]),
        ..Default::default()
    };
    eframe::run_native(
        "its4tasks",
        options,
        Box::new(|cc| Ok(Box::new(app::TodoApp::new(cc)))),
    )
}
