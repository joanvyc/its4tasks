//! The its4tasks GUI. It shows a board kept by `its4tasks-server`: natively,
//! a server's or a local one (`native::shell` asks which), and in the
//! browser, the board of the server that served the page.

#[cfg(not(target_arch = "wasm32"))]
const USAGE: &str = "usage: its4tasks [--get-board]
  --get-board  print the board as an its4tasks.json, without opening a window";

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] => {}
        ["--get-board"] => match its4tasks::native::cli::get_board() {
            Ok(json) => {
                println!("{json}");
                return Ok(());
            }
            Err(e) => {
                eprintln!("its4tasks: {e}");
                std::process::exit(1);
            }
        },
        ["--help" | "-h"] => {
            println!("{USAGE}");
            return Ok(());
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
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
        Box::new(|cc| Ok(Box::new(its4tasks::native::shell::Shell::new(cc)))),
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
                Box::new(|cc| Ok(Box::new(its4tasks::app::TodoApp::new(cc)))),
            )
            .await;
        if let Err(e) = started {
            eframe::web_sys::console::error_1(&e);
        }
    });
}
