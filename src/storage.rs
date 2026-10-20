use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use crate::model::Board;

/// `~/.local/share/its4tasks/board.json` on Linux, the platform equivalent elsewhere.
pub fn data_path() -> Option<PathBuf> {
    ProjectDirs::from("", "", "its4tasks").map(|d| d.data_dir().join("board.json"))
}

/// Where the app kept its board when it was called focal.
fn legacy_data_path() -> Option<PathBuf> {
    ProjectDirs::from("", "", "focal").map(|d| d.data_dir().join("board.json"))
}

/// Moves a board left by the old focal name to `path`, if there is one and `path` is free.
fn migrate_legacy(path: &Path) {
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

pub fn load() -> Board {
    let Some(path) = data_path() else {
        eprintln!("its4tasks: could not determine a data directory; starting empty");
        return Board::default();
    };
    migrate_legacy(&path);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Board::default(),
        Err(e) => {
            eprintln!("its4tasks: failed to read {}: {e}", path.display());
            return Board::default();
        }
    };
    match serde_json::from_str::<Board>(&text) {
        Ok(mut board) => {
            board.sanitize();
            board
        }
        Err(e) => {
            // Keep the unreadable file around instead of overwriting it on the next save.
            let backup = path.with_extension("json.corrupt");
            eprintln!(
                "its4tasks: failed to parse {}: {e}; moved it to {}",
                path.display(),
                backup.display()
            );
            let _ = fs::rename(&path, &backup);
            Board::default()
        }
    }
}

/// Write to a temp file then rename, so a crash can't leave a half-written file.
pub fn save(board: &Board) -> io::Result<()> {
    let path = data_path().ok_or_else(|| io::Error::other("no data directory"))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(board).map_err(io::Error::other)?;
    fs::write(&tmp, json)?;
    fs::rename(&tmp, &path)
}
