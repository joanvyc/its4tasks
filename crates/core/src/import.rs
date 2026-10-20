//! Boards from `board.json` files: exported ones, and the files older
//! versions of the app kept their board in.

use std::collections::HashMap;

use serde_json::Value;
use uuid::Uuid;

use crate::model::{Board, NextStep};

/// The board in a `board.json` file's text, with dangling references
/// dropped. Accepts the older formats: numbered tasks, and notes as one
/// string.
pub fn parse(text: &str) -> Result<Board, String> {
    let mut value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    number_ids_to_uuids(&mut value);
    let mut board: Board = serde_json::from_value(value).map_err(|e| e.to_string())?;
    board.sanitize();
    Ok(board)
}

/// The same board with new task and comment ids, so it can join another
/// board without clashing, even with itself. Tasks get v7 ids in list order,
/// keeping that order; parents, slots and promoted steps follow.
pub fn fresh_ids(mut board: Board) -> Board {
    let ids: HashMap<Uuid, Uuid> = board.tasks.iter().map(|t| (t.id, Uuid::now_v7())).collect();
    // A reference to a task that isn't in the board stays unique too.
    let map = |id: Uuid| ids.get(&id).copied().unwrap_or_else(Uuid::now_v7);
    for task in &mut board.tasks {
        task.id = map(task.id);
        task.parent = task.parent.map(map);
        for note in &mut task.notes {
            note.id = Uuid::new_v4();
            if let Some(NextStep::Promoted { subtask, .. }) = &mut note.next_step {
                *subtask = map(*subtask);
            }
        }
    }
    for slot in board.slots.iter_mut().flatten() {
        *slot = map(*slot);
    }
    board
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
    use chrono::Utc;

    use super::*;
    use crate::model::MessageKind;

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

    #[test]
    fn parses_old_and_current_files() {
        let old = r#"{"tasks": [{"id": 0, "title": "a", "notes": "  old notes "}],
                      "slots": [0, null, null, null], "next_id": 1}"#;
        let board = parse(old).unwrap();
        assert_eq!(board.tasks[0].notes[0].text, "old notes");
        assert_eq!(board.slots[0], Some(board.tasks[0].id));
        let again = parse(&serde_json::to_string(&board).unwrap()).unwrap();
        assert_eq!(again, board);
        assert!(parse("{ not json").is_err());
    }

    #[test]
    fn fresh_ids_keep_the_shape() {
        let now = Utc::now();
        let mut b = Board::default();
        let a = b.add_task("a".into(), "first", None, now);
        let s = b.add_task("b".into(), "", Some(a), now);
        b.task_mut(a).unwrap().post("next", MessageKind::Next, now);
        b.focus(s);
        let idx = b.task(a).unwrap().notes.len() - 1;
        b.promote_step(a, idx, "c".into(), "", now).unwrap();

        let fresh = fresh_ids(b.clone());
        let (fa, fs) = (fresh.tasks[0].id, fresh.tasks[1].id);
        assert!(fa != a && fs != s && fa < fs);
        assert_eq!(fresh.tasks[1].parent, Some(fa));
        assert_eq!(fresh.slots[0], Some(fs));
        assert_ne!(fresh.tasks[0].notes[0].id, b.tasks[0].notes[0].id);
        let Some(NextStep::Promoted { subtask, .. }) = fresh.tasks[0].notes[idx].next_step else {
            panic!("not promoted");
        };
        assert_eq!(subtask, fresh.tasks[2].id);
        // Everything else is the same.
        let titles = |b: &Board| b.tasks.iter().map(|t| t.title.clone()).collect::<Vec<_>>();
        assert_eq!(titles(&fresh), titles(&b));
    }
}
