//! The only way the UI touches the board: it reads a `&Board` and sends
//! `Change`s. `Change` is serializable so it can later travel over HTTP.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
        task: u64,
        idx: usize,
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
        task: u64,
        idx: usize,
        text: String,
    },
    DeleteNote {
        task: u64,
        idx: usize,
    },
    MarkBlocking {
        task: u64,
        idx: usize,
    },
    Resolve {
        task: u64,
        idx: usize,
    },
    Unblock {
        task: u64,
        idx: usize,
    },
    MarkStep {
        task: u64,
        idx: usize,
    },
    ToggleStep {
        task: u64,
        idx: usize,
    },
    UnmarkStep {
        task: u64,
        idx: usize,
    },
    ToggleStar {
        task: u64,
        idx: usize,
    },
    MoveNote {
        from: u64,
        idx: usize,
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
    UnknownNote(u64, usize),
    Move(MoveError),
    /// The board's rules don't allow it, e.g. a parent that makes a cycle.
    Refused,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ApiError::UnknownTask(id) => write!(f, "There is no task {id}"),
            ApiError::UnknownNote(id, idx) => write!(f, "Task {id} has no comment {idx}"),
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
            task: id,
            idx,
            title,
            notes,
            focus,
        } => {
            note(b, id, idx)?;
            let id = b
                .promote_step(id, idx, title, &notes, now)
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
        Change::EditNote {
            task: id,
            idx,
            text,
        } => note(b, id, idx)?.edit_note(idx, &text, now),
        Change::DeleteNote { task: id, idx } => note(b, id, idx)?.delete_note(idx),
        Change::MarkBlocking { task: id, idx } => note(b, id, idx)?.set_blocking(idx, now),
        Change::Resolve { task: id, idx } => note(b, id, idx)?.resolve(idx, now),
        Change::Unblock { task: id, idx } => note(b, id, idx)?.unmark_blocking(idx),
        Change::MarkStep { task: id, idx } => note(b, id, idx)?.mark_next_step(idx, now),
        Change::ToggleStep { task: id, idx } => note(b, id, idx)?.toggle_step(idx, now),
        Change::UnmarkStep { task: id, idx } => note(b, id, idx)?.unmark_next_step(idx),
        Change::ToggleStar { task: id, idx } => note(b, id, idx)?.toggle_star(idx),
        Change::MoveNote { from, idx, to } => b.move_note(from, idx, to).map_err(ApiError::Move)?,
    }
    Ok(Outcome::Done)
}

fn task(b: &mut Board, id: u64) -> Result<&mut Task, ApiError> {
    b.task_mut(id).ok_or(ApiError::UnknownTask(id))
}

/// The task, if it has a comment at `idx`.
fn note(b: &mut Board, id: u64, idx: usize) -> Result<&mut Task, ApiError> {
    let t = task(b, id)?;
    if idx < t.notes.len() {
        Ok(t)
    } else {
        Err(ApiError::UnknownNote(id, idx))
    }
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
            from: a,
            idx: 0,
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
        assert_eq!(
            api.apply(Change::ToggleStar { task: a, idx: 5 }),
            Err(ApiError::UnknownNote(a, 5))
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
