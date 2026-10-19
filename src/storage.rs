use std::fs;
use std::io;
use std::path::PathBuf;

use directories::ProjectDirs;

use crate::model::Board;

/// `~/.local/share/focal/board.json` on Linux, the platform equivalent elsewhere.
pub fn data_path() -> Option<PathBuf> {
    ProjectDirs::from("", "", "focal").map(|d| d.data_dir().join("board.json"))
}

pub fn load() -> Board {
    let Some(path) = data_path() else {
        eprintln!("focal: could not determine a data directory; starting empty");
        return Board::default();
    };
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Board::default(),
        Err(e) => {
            eprintln!("focal: failed to read {}: {e}", path.display());
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
                "focal: failed to parse {}: {e}; moved it to {}",
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
