//! Files of the board: picking an `its4tasks.json` (or an older version's
//! `board.json`) to import, and saving one when exporting. Natively through
//! the system's dialogs; in the browser, its file picker and a download.

use std::sync::mpsc::{self, Receiver};

use crate::import;
use crate::model::Board;

/// What came of picking a file: `None` if it was cancelled.
pub type Picked = Option<Result<Board, String>>;

/// What came of saving a file: `None` if it was cancelled.
pub type Saved = Option<Result<(), String>>;

/// The name exported files get.
pub const EXPORT_NAME: &str = "its4tasks.json";

/// Asks for an `its4tasks.json` and reads it. The answer arrives on the
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
            .set_title("Import an its4tasks.json")
            .add_filter("its4tasks", &["json"])
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
            .set_title("Import an its4tasks.json")
            .add_filter("its4tasks", &["json"])
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

/// Saves `json` as an `its4tasks.json`: natively where the user picks, in
/// the browser as a download. The answer arrives on the receiver, and
/// `wake` is called then.
pub fn save_export(json: String, wake: impl Fn() + Send + 'static) -> Receiver<Saved> {
    let (tx, rx) = mpsc::channel();
    let done = move |saved: Saved| {
        let _ = tx.send(saved);
        wake();
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(move || {
        let path = rfd::FileDialog::new()
            .set_title("Export your board")
            .set_file_name(EXPORT_NAME)
            .add_filter("its4tasks", &["json"])
            .save_file();
        done(path.map(|path| {
            std::fs::write(&path, json).map_err(|e| format!("{}: {e}", path.display()))
        }));
    });
    #[cfg(target_arch = "wasm32")]
    done(Some(download(&json)));
    rx
}

/// Has the browser download `json` as `EXPORT_NAME`, through a link to it
/// that's clicked at once. (rfd's save would show a dialog of its own.)
#[cfg(target_arch = "wasm32")]
fn download(json: &str) -> Result<(), String> {
    use eframe::wasm_bindgen::{JsCast, JsValue};
    use web_sys::{Blob, BlobPropertyBag, HtmlAnchorElement, Url};

    let failed = |e: JsValue| format!("{e:?}");
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no page to download from")?;
    let parts = js_sys::Array::of1(&JsValue::from_str(json));
    let options = BlobPropertyBag::new();
    options.set_type("application/json");
    let blob = Blob::new_with_str_sequence_and_options(&parts, &options).map_err(failed)?;
    let url = Url::create_object_url_with_blob(&blob).map_err(failed)?;
    let link: HtmlAnchorElement = document
        .create_element("a")
        .map_err(failed)?
        .dyn_into()
        .map_err(|_| "not a link".to_owned())?;
    link.set_href(&url);
    link.set_download(EXPORT_NAME);
    let body = document.body().ok_or("no page to download from")?;
    body.append_child(&link).map_err(failed)?;
    link.click();
    link.remove();
    Url::revoke_object_url(&url).map_err(failed)
}
