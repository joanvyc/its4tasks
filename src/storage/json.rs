//! The JSON file the board lived in before SQLite, imported once.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde_json::Value;
use uuid::Uuid;

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
    let board = serde_json::from_str::<Value>(&text).and_then(|mut value| {
        number_ids_to_uuids(&mut value);
        serde_json::from_value::<Board>(value)
    });
    match board {
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

/// Files from before task UUIDs number their tasks. Gives each number a v7
/// UUID, in list order so the tasks stay in that order, and rewrites the
/// task ids, parents, slots and promoted steps' subtasks to match.
fn number_ids_to_uuids(board: &mut Value) {
    let mut ids: HashMap<u64, Uuid> = HashMap::new();
    let mut map = |v: &mut Value| {
        if let Some(n) = v.as_u64() {
            *v = Value::String(ids.entry(n).or_insert_with(Uuid::now_v7).to_string());
        }
    };
    let tasks = board.get_mut("tasks").and_then(Value::as_array_mut);
    for task in tasks.into_iter().flatten() {
        task.get_mut("id").map(&mut map);
    }
    let tasks = board.get_mut("tasks").and_then(Value::as_array_mut);
    for task in tasks.into_iter().flatten() {
        task.get_mut("parent").map(&mut map);
        let notes = task.get_mut("notes").and_then(Value::as_array_mut);
        for note in notes.into_iter().flatten() {
            note.pointer_mut("/next_step/Promoted/subtask")
                .map(&mut map);
        }
    }
    let slots = board.get_mut("slots").and_then(Value::as_array_mut);
    for slot in slots.into_iter().flatten() {
        map(slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_tasks_get_uuids_in_order() {
        let mut v: Value = serde_json::from_str(
            r#"{"tasks": [
                {"id": 7, "notes": []},
                {"id": 2, "parent": 7, "notes": [
                    {"next_step": {"Promoted": {"subtask": 9}}}
                ]}
            ], "slots": [2, null, null, null], "next_id": 10}"#,
        )
        .unwrap();
        number_ids_to_uuids(&mut v);
        let id = |p: &str| v.pointer(p).and_then(Value::as_str).unwrap().to_owned();
        let (first, second) = (id("/tasks/0/id"), id("/tasks/1/id"));
        assert!(first < second, "list order kept");
        assert_eq!(id("/tasks/1/parent"), first);
        assert_eq!(id("/slots/0"), second);
        let subtask = id("/tasks/1/notes/0/next_step/Promoted/subtask");
        assert!(subtask != first && subtask != second);
        assert!(v.pointer("/slots/1").unwrap().is_null());
    }
}
