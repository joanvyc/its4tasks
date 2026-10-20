//! The its4tasks GUI. It shows the board kept by `its4tasks-server`:
//! natively at `$ITS4TASKS_SERVER` (by default http://127.0.0.1:7878), and in
//! the browser on the server that served the page.

use its4tasks::app::TodoApp;

#[cfg(not(target_arch = "wasm32"))]
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
        Box::new(|cc| Ok(Box::new(TodoApp::new(cc)))),
    )
}

/// Runs in the `<canvas id="its4tasks">` of `index.html`.
#[cfg(target_arch = "wasm32")]
fn main() {
    use eframe::wasm_bindgen::JsCast;

    let canvas = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("its4tasks"))
        .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        .expect("index.html has a <canvas id=\"its4tasks\">");
    wasm_bindgen_futures::spawn_local(async move {
        let started = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| Ok(Box::new(TodoApp::new(cc)))),
            )
            .await;
        if let Err(e) = started {
            eframe::web_sys::console::error_1(&e);
        }
    });
}
