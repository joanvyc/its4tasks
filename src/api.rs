//! The only way the UI touches the board: it reads a `&Board` and sends
//! `Change`s. `Change` is serializable so it can later travel over HTTP.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::{Board, MessageKind, MoveError, Task};
use crate::storage::Store;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Change {
    AddTask {
        title: String,
        notes: String,
        parent: Option<u64>,
        focus: bool,
    },
    /// Turn a next step into a subtask of its task.
    PromoteStep {
        note: Uuid,
        title: String,
        notes: String,
        focus: bool,
    },
    Rename {
        task: u64,
        title: String,
    },
    SetDone {
        task: u64,
        done: bool,
    },
    ToggleFold {
        task: u64,
    },
    /// Deletes its subtasks too.
    DeleteTask {
        task: u64,
    },
    SetParent {
        task: u64,
        parent: Option<u64>,
    },
    Focus {
        task: u64,
    },
    FocusInto {
        slot: usize,
        task: u64,
    },
    Unfocus {
        slot: usize,
    },
    Post {
        task: u64,
        text: String,
        kind: MessageKind,
    },
    EditNote {
        note: Uuid,
        text: String,
    },
    DeleteNote {
        note: Uuid,
    },
    MarkBlocking {
        note: Uuid,
    },
    Resolve {
        note: Uuid,
    },
    Unblock {
        note: Uuid,
    },
    MarkStep {
        note: Uuid,
    },
    ToggleStep {
        note: Uuid,
    },
    UnmarkStep {
        note: Uuid,
    },
    ToggleStar {
        note: Uuid,
    },
    MoveNote {
        note: Uuid,
        to: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Created(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiError {
    UnknownTask(u64),
    UnknownNote(Uuid),
    Move(MoveError),
    /// The board's rules don't allow it, e.g. a parent that makes a cycle.
    Refused,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ApiError::UnknownTask(id) => write!(f, "There is no task {id}"),
            ApiError::UnknownNote(id) => write!(f, "There is no comment {id}"),
            ApiError::Move(e) => e.fmt(f),
            ApiError::Refused => f.write_str("Not allowed"),
        }
    }
}

pub struct Api {
    board: Board,
    /// `None` if the database couldn't be opened; changes then aren't saved.
    store: Option<Store>,
}

impl Api {
    /// The board saved in the data directory.
    pub fn open() -> Api {
        let store = Store::open()
            .inspect_err(|e| {
                eprintln!("its4tasks: failed to open the database: {e}; changes won't be saved")
            })
            .ok();
        Api {
            board: store.as_ref().map(Store::load).unwrap_or_default(),
            store,
        }
    }

    /// A board that isn't saved anywhere.
    #[cfg(test)]
    pub fn new(board: Board) -> Api {
        Api { board, store: None }
    }

    pub fn board(&self) -> &Board {
        &self.board
    }

    pub fn apply(&mut self, change: Change) -> Result<Outcome, ApiError> {
        self.apply_at(change, Utc::now())
    }

    fn apply_at(&mut self, change: Change, now: DateTime<Utc>) -> Result<Outcome, ApiError> {
        let outcome = change_board(&mut self.board, change, now)?;
        if let Some(store) = &mut self.store
            && let Err(e) = store.save(&self.board)
        {
            eprintln!("its4tasks: failed to save: {e}");
        }
        Ok(outcome)
    }
}

fn change_board(b: &mut Board, change: Change, now: DateTime<Utc>) -> Result<Outcome, ApiError> {
    match change {
        Change::AddTask {
            title,
            notes,
            parent,
            focus,
        } => {
            if let Some(p) = parent {
                task(b, p)?;
            }
            let id = b.add_task(title, &notes, parent, now);
            if focus {
                b.focus(id);
            }
            return Ok(Outcome::Created(id));
        }
        Change::PromoteStep {
            note: note_id,
            title,
            notes,
            focus,
        } => {
            let (task_id, idx) = find(b, note_id)?;
            let id = b
                .promote_step(task_id, idx, title, &notes, now)
                .ok_or(ApiError::Refused)?;
            if focus {
                b.focus(id);
            }
            return Ok(Outcome::Created(id));
        }
        Change::Rename { task: id, title } => {
            let t = task(b, id)?;
            t.title = title;
            t.touch(now);
        }
        Change::SetDone { task: id, done } => task(b, id)?.set_done(done, now),
        Change::ToggleFold { task: id } => task(b, id)?.toggle_fold(),
        Change::DeleteTask { task: id } => {
            task(b, id)?;
            b.delete_task(id);
        }
        Change::SetParent { task: id, parent } => {
            task(b, id)?;
            if !b.set_parent(id, parent) {
                return Err(ApiError::Refused);
            }
        }
        Change::Focus { task: id } => {
            task(b, id)?;
            if !b.focus(id) {
                return Err(ApiError::Refused);
            }
        }
        Change::FocusInto { slot, task: id } => {
            task(b, id)?;
            b.focus_into(slot, id);
        }
        Change::Unfocus { slot } => b.unfocus(slot),
        Change::Post {
            task: id,
            text,
            kind,
        } => task(b, id)?.post(&text, kind, now),
        Change::EditNote { note: id, text } => {
            let (t, idx) = note(b, id)?;
            t.edit_note(idx, &text, now);
        }
        Change::DeleteNote { note: id } => {
            let (t, idx) = note(b, id)?;
            t.delete_note(idx);
        }
        Change::MarkBlocking { note: id } => {
            let (t, idx) = note(b, id)?;
            t.set_blocking(idx, now);
        }
        Change::Resolve { note: id } => {
            let (t, idx) = note(b, id)?;
            t.resolve(idx, now);
        }
        Change::Unblock { note: id } => {
            let (t, idx) = note(b, id)?;
            t.unmark_blocking(idx);
        }
        Change::MarkStep { note: id } => {
            let (t, idx) = note(b, id)?;
            t.mark_next_step(idx, now);
        }
        Change::ToggleStep { note: id } => {
            let (t, idx) = note(b, id)?;
            t.toggle_step(idx, now);
        }
        Change::UnmarkStep { note: id } => {
            let (t, idx) = note(b, id)?;
            t.unmark_next_step(idx);
        }
        Change::ToggleStar { note: id } => {
            let (t, idx) = note(b, id)?;
            t.toggle_star(idx);
        }
        Change::MoveNote { note: id, to } => {
            let (from, idx) = find(b, id)?;
            b.move_note(from, idx, to).map_err(ApiError::Move)?;
        }
    }
    Ok(Outcome::Done)
}

fn task(b: &mut Board, id: u64) -> Result<&mut Task, ApiError> {
    b.task_mut(id).ok_or(ApiError::UnknownTask(id))
}

/// The task holding a comment, and the comment's index in it.
fn find(b: &Board, id: Uuid) -> Result<(u64, usize), ApiError> {
    b.find_note(id).ok_or(ApiError::UnknownNote(id))
}

/// Like `find`, with the task ready to change.
fn note(b: &mut Board, id: Uuid) -> Result<(&mut Task, usize), ApiError> {
    let (task_id, idx) = find(b, id)?;
    Ok((task(b, task_id)?, idx))
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;

    fn add(api: &mut Api, title: &str, focus: bool) -> u64 {
        let change = Change::AddTask {
            title: title.into(),
            notes: String::new(),
            parent: None,
            focus,
        };
        match api.apply(change) {
            Ok(Outcome::Created(id)) => id,
            other => panic!("expected a new task, got {other:?}"),
        }
    }

    #[test]
    fn add_task_can_focus_it() {
        let mut api = Api::new(Board::default());
        let a = add(&mut api, "a", true);
        let b = add(&mut api, "b", false);
        assert_eq!(api.board().slot_of(a), Some(0));
        assert_eq!(api.board().slot_of(b), None);
    }

    #[test]
    fn rename_touches_the_task() {
        let mut api = Api::new(Board::default());
        let id = add(&mut api, "a", false);
        let later = Utc::now() + TimeDelta::hours(1);
        let rename = Change::Rename {
            task: id,
            title: "b".into(),
        };
        assert_eq!(api.apply_at(rename, later), Ok(Outcome::Done));
        let t = api.board().task(id).unwrap();
        assert_eq!((t.title.as_str(), t.updated_at), ("b", later));
    }

    #[test]
    fn reports_why_a_change_failed() {
        let mut api = Api::new(Board::default());
        let a = add(&mut api, "a", false);
        let b = add(&mut api, "b", false);
        let post = Change::Post {
            task: a,
            text: "hi".into(),
            kind: MessageKind::Comment,
        };
        api.apply(post).unwrap();
        api.apply(Change::SetDone {
            task: b,
            done: true,
        })
        .unwrap();
        let moved = Change::MoveNote {
            note: api.board().task(a).unwrap().notes[0].id,
            to: b,
        };
        assert_eq!(
            api.apply(moved),
            Err(ApiError::Move(MoveError::TargetClosed))
        );
        assert_eq!(
            api.apply(Change::ToggleFold { task: 99 }),
            Err(ApiError::UnknownTask(99))
        );
        let gone = Uuid::new_v4();
        assert_eq!(
            api.apply(Change::ToggleStar { note: gone }),
            Err(ApiError::UnknownNote(gone))
        );
    }

    #[test]
    fn comments_keep_their_id_when_others_go() {
        let mut api = Api::new(Board::default());
        let a = add(&mut api, "a", false);
        for text in ["first", "second"] {
            let post = Change::Post {
                task: a,
                text: text.into(),
                kind: MessageKind::Comment,
            };
            api.apply(post).unwrap();
        }
        let ids: Vec<Uuid> = api
            .board()
            .task(a)
            .unwrap()
            .notes
            .iter()
            .map(|n| n.id)
            .collect();
        // With indices, the edit would have landed on "first"'s old slot.
        api.apply(Change::DeleteNote { note: ids[0] }).unwrap();
        let edit = Change::EditNote {
            note: ids[1],
            text: "second, edited".into(),
        };
        api.apply(edit).unwrap();
        let notes = &api.board().task(a).unwrap().notes;
        assert_eq!(notes.len(), 1);
        assert_eq!(
            (notes[0].id, notes[0].text.as_str()),
            (ids[1], "second, edited")
        );
        assert_eq!(
            api.apply(Change::ToggleStar { note: ids[0] }),
            Err(ApiError::UnknownNote(ids[0]))
        );
    }

    #[test]
    fn changes_round_trip_through_json() {
        let change = Change::Post {
            task: 3,
            text: "/block waiting".into(),
            kind: MessageKind::Block,
        };
        let json = serde_json::to_string(&change).unwrap();
        assert_eq!(serde_json::from_str::<Change>(&json).unwrap(), change);
    }
}
