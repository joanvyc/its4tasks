mod api;
mod app;
mod log;
mod model;
mod storage;

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
