//! The JSON file the board lived in before SQLite, imported once.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use crate::model::Board;

pub const FILE_NAME: &str = "board.json";

/// Where the app kept its board when it was called focal.
fn legacy_data_path() -> Option<PathBuf> {
    ProjectDirs::from("", "", "focal").map(|d| d.data_dir().join(FILE_NAME))
}

/// Moves a board left by the old focal name to `path`, if there is one and `path` is free.
pub fn migrate_legacy(path: &Path) {
    let Some(legacy) = legacy_data_path() else {
        return;
    };
    if path.exists() || !legacy.exists() {
        return;
    }
    let moved = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::rename(&legacy, path).or_else(|_| fs::copy(&legacy, path).map(drop)));
    match moved {
        Ok(()) => eprintln!(
            "its4tasks: moved {} to {}",
            legacy.display(),
            path.display()
        ),
        Err(e) => eprintln!(
            "its4tasks: failed to move {} to {}: {e}",
            legacy.display(),
            path.display()
        ),
    }
}

/// The board in the file at `path`, if there is a readable one.
pub fn read(path: &Path) -> Option<Board> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!("its4tasks: failed to read {}: {e}", path.display());
            return None;
        }
    };
    match serde_json::from_str::<Board>(&text) {
        Ok(mut board) => {
            board.sanitize();
            Some(board)
        }
        Err(e) => {
            // Keep the unreadable file around, out of the way of the next import.
            let backup = path.with_extension("json.corrupt");
            eprintln!(
                "its4tasks: failed to parse {}: {e}; moved it to {}",
                path.display(),
                backup.display()
            );
            let _ = fs::rename(path, &backup);
            None
        }
    }
}

/// Renames the file once its board is in the database, so it isn't imported again.
pub fn mark_imported(path: &Path) {
    let done = path.with_extension("json.imported");
    match fs::rename(path, &done) {
        Ok(()) => eprintln!(
            "its4tasks: imported {} into the database; kept it as {}",
            path.display(),
            done.display()
        ),
        Err(e) => eprintln!(
            "its4tasks: imported {} but failed to rename it: {e}",
            path.display()
        ),
    }
}
