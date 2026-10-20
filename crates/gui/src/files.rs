//! Picking a `board.json` to import: a native dialog, or the browser's.

use std::sync::mpsc::{self, Receiver};

use crate::import;
use crate::model::Board;

/// What came of picking a file: `None` if it was cancelled.
pub type Picked = Option<Result<Board, String>>;

/// Asks for a `board.json` and reads it. The answer arrives on the
/// receiver, and `wake` is called then.
pub fn pick_board(wake: impl Fn() + Send + 'static) -> Receiver<Picked> {
    let (tx, rx) = mpsc::channel();
    let done = move |picked: Picked| {
        let _ = tx.send(picked);
        wake();
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(move || {
        let path = rfd::FileDialog::new()
            .set_title("Import a board")
            .add_filter("Board", &["json"])
            .pick_file();
        done(path.map(|path| {
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            import::parse(&text)
        }));
    });
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let file = rfd::AsyncFileDialog::new()
            .set_title("Import a board")
            .add_filter("Board", &["json"])
            .pick_file()
            .await;
        let picked = match file {
            Some(file) => {
                let bytes = file.read().await;
                Some(
                    String::from_utf8(bytes)
                        .map_err(|e| e.to_string())
                        .and_then(|text| import::parse(&text)),
                )
            }
            None => None,
        };
        done(picked);
    });
    rx
}
