//! Prints the `Change` that imports a `board.json` (read from the file
//! named, or stdin), for POSTing to `/api/changes`, say to seed a test
//! user's board:
//!
//! ```sh
//! cargo run -q -p its4tasks-core --example import_change -- board.json
//! ```

use std::io::Read;

use its4tasks_core::api::Change;
use its4tasks_core::import;

fn main() -> Result<(), String> {
    let text = match std::env::args().nth(1) {
        Some(path) => std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?,
        None => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|e| e.to_string())?;
            text
        }
    };
    let board = import::fresh_ids(import::parse(&text)?);
    let change = Change::Import { board };
    println!(
        "{}",
        serde_json::to_string(&change).expect("a Change serializes")
    );
    Ok(())
}
