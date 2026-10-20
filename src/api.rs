//! The only way to change the board: a client reads a `Board` and sends
//! `Change`s, which travel over HTTP as JSON (see `server`).
//!
//! `apply_change` holds the rules. The server applies a change with it, and
//! the GUI applies the same change to its own copy straight away (see
//! `client`). For both to agree, changes that create a task or comment carry
//! the new ids, picked by the client.

use std::fmt;

use chrono::{DateTime, Utc};
use sea_orm::DbErr;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::{Board, MessageKind, MoveError, Task};
use crate::storage::Store;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Change {
    /// `id` is the new task's, `note_id` its first comment's (if `notes`
    /// isn't blank).
    AddTask {
        id: Uuid,
        note_id: Uuid,
        title: String,
        notes: String,
        parent: Option<Uuid>,
        focus: bool,
    },
    /// Turn a next step into a subtask of its task, with ids as in `AddTask`.
    PromoteStep {
        note: Uuid,
        id: Uuid,
        note_id: Uuid,
        title: String,
        notes: String,
        focus: bool,
    },
    Rename {
        task: Uuid,
        title: String,
    },
    SetDone {
        task: Uuid,
        done: bool,
    },
    ToggleFold {
        task: Uuid,
    },
    /// Deletes its subtasks too.
    DeleteTask {
        task: Uuid,
    },
    SetParent {
        task: Uuid,
        parent: Option<Uuid>,
    },
    Focus {
        task: Uuid,
    },
    FocusInto {
        slot: usize,
        task: Uuid,
    },
    Unfocus {
        slot: usize,
    },
    /// `id` is the new comment's.
    Post {
        task: Uuid,
        id: Uuid,
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
        to: Uuid,
    },
}

impl Change {
    /// An `AddTask` with fresh ids.
    pub fn add_task(title: String, notes: String, parent: Option<Uuid>, focus: bool) -> Change {
        Change::AddTask {
            // Time-ordered, so sorting by id keeps tasks in creation order.
            id: Uuid::now_v7(),
            note_id: Uuid::new_v4(),
            title,
            notes,
            parent,
            focus,
        }
    }

    /// A `PromoteStep` with fresh ids.
    pub fn promote_step(note: Uuid, title: String, notes: String, focus: bool) -> Change {
        Change::PromoteStep {
            note,
            id: Uuid::now_v7(),
            note_id: Uuid::new_v4(),
            title,
            notes,
            focus,
        }
    }

    /// A `Post` with a fresh id.
    pub fn post(task: Uuid, text: String, kind: MessageKind) -> Change {
        Change::Post {
            task,
            id: Uuid::new_v4(),
            text,
            kind,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Done,
    Created(Uuid),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApiError {
    UnknownTask(Uuid),
    UnknownNote(Uuid),
    Move(MoveError),
    /// A new task or comment was given an id that's taken.
    Duplicate(Uuid),
    /// The board's rules don't allow it, e.g. a parent that makes a cycle.
    Refused,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ApiError::UnknownTask(id) => write!(f, "There is no task {id}"),
            ApiError::UnknownNote(id) => write!(f, "There is no comment {id}"),
            ApiError::Move(e) => e.fmt(f),
            ApiError::Duplicate(id) => write!(f, "The id {id} is taken"),
            ApiError::Refused => f.write_str("Not allowed"),
        }
    }
}

/// The board and its rules, kept by the server. Clients reach it over HTTP
/// (see `server` and `client`).
pub struct Api {
    board: Board,
    /// `None` for a board that isn't saved anywhere.
    store: Option<Store>,
}

impl Api {
    /// The board saved in the data directory.
    pub async fn open() -> Result<Api, DbErr> {
        let store = Store::open().await?;
        Ok(Api {
            board: store.load(),
            store: Some(store),
        })
    }

    /// A board that isn't saved anywhere.
    #[cfg(test)]
    pub fn new(board: Board) -> Api {
        Api { board, store: None }
    }

    pub fn board(&self) -> &Board {
        &self.board
    }

    pub async fn apply(&mut self, change: Change) -> Result<Outcome, ApiError> {
        self.apply_at(change, Utc::now()).await
    }

    async fn apply_at(&mut self, change: Change, now: DateTime<Utc>) -> Result<Outcome, ApiError> {
        let outcome = apply_change(&mut self.board, change, now)?;
        if let Some(store) = &mut self.store
            && let Err(e) = store.save(&self.board).await
        {
            eprintln!("its4tasks: failed to save: {e}");
        }
        Ok(outcome)
    }
}

/// Applies a change to a board as of `now`: the board's rules, shared by the
/// server and the GUI's optimistic copy. The same change on equal boards at
/// the same `now` gives equal boards.
pub fn apply_change(
    b: &mut Board,
    change: Change,
    now: DateTime<Utc>,
) -> Result<Outcome, ApiError> {
    match change {
        Change::AddTask {
            id,
            note_id,
            title,
            notes,
            parent,
            focus,
        } => {
            if let Some(p) = parent {
                task(b, p)?;
            }
            unused_task(b, id)?;
            unused_note(b, note_id)?;
            b.add_task_as(id, note_id, title, &notes, parent, now);
            if focus {
                b.focus(id);
            }
            return Ok(Outcome::Created(id));
        }
        Change::PromoteStep {
            note: step,
            id,
            note_id,
            title,
            notes,
            focus,
        } => {
            let (task_id, idx) = find(b, step)?;
            unused_task(b, id)?;
            unused_note(b, note_id)?;
            if !b.promote_step_as(id, note_id, task_id, idx, title, &notes, now) {
                return Err(ApiError::Refused);
            }
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
            task: task_id,
            id,
            text,
            kind,
        } => {
            unused_note(b, id)?;
            task(b, task_id)?.post_as(id, &text, kind, now);
        }
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

fn unused_task(b: &Board, id: Uuid) -> Result<(), ApiError> {
    match b.task(id) {
        Some(_) => Err(ApiError::Duplicate(id)),
        None => Ok(()),
    }
}

fn unused_note(b: &Board, id: Uuid) -> Result<(), ApiError> {
    match b.find_note(id) {
        Some(_) => Err(ApiError::Duplicate(id)),
        None => Ok(()),
    }
}

fn task(b: &mut Board, id: Uuid) -> Result<&mut Task, ApiError> {
    b.task_mut(id).ok_or(ApiError::UnknownTask(id))
}

/// The task holding a comment, and the comment's index in it.
fn find(b: &Board, id: Uuid) -> Result<(Uuid, usize), ApiError> {
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

    async fn add(api: &mut Api, title: &str, focus: bool) -> Uuid {
        let change = Change::add_task(title.into(), String::new(), None, focus);
        match api.apply(change).await {
            Ok(Outcome::Created(id)) => id,
            other => panic!("expected a new task, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn add_task_can_focus_it() {
        let mut api = Api::new(Board::default());
        let a = add(&mut api, "a", true).await;
        let b = add(&mut api, "b", false).await;
        assert_eq!(api.board().slot_of(a), Some(0));
        assert_eq!(api.board().slot_of(b), None);
    }

    #[tokio::test]
    async fn rename_touches_the_task() {
        let mut api = Api::new(Board::default());
        let id = add(&mut api, "a", false).await;
        let later = Utc::now() + TimeDelta::hours(1);
        let rename = Change::Rename {
            task: id,
            title: "b".into(),
        };
        assert_eq!(api.apply_at(rename, later).await, Ok(Outcome::Done));
        let t = api.board().task(id).unwrap();
        assert_eq!((t.title.as_str(), t.updated_at), ("b", later));
    }

    #[tokio::test]
    async fn reports_why_a_change_failed() {
        let mut api = Api::new(Board::default());
        let a = add(&mut api, "a", false).await;
        let b = add(&mut api, "b", false).await;
        let post = Change::post(a, "hi".into(), MessageKind::Comment);
        api.apply(post).await.unwrap();
        api.apply(Change::SetDone {
            task: b,
            done: true,
        })
        .await
        .unwrap();
        let moved = Change::MoveNote {
            note: api.board().task(a).unwrap().notes[0].id,
            to: b,
        };
        assert_eq!(
            api.apply(moved).await,
            Err(ApiError::Move(MoveError::TargetClosed))
        );
        assert_eq!(
            api.apply(Change::ToggleFold {
                task: Uuid::from_u128(99)
            })
            .await,
            Err(ApiError::UnknownTask(Uuid::from_u128(99)))
        );
        let gone = Uuid::new_v4();
        assert_eq!(
            api.apply(Change::ToggleStar { note: gone }).await,
            Err(ApiError::UnknownNote(gone))
        );
    }

    #[tokio::test]
    async fn comments_keep_their_id_when_others_go() {
        let mut api = Api::new(Board::default());
        let a = add(&mut api, "a", false).await;
        for text in ["first", "second"] {
            let post = Change::post(a, text.into(), MessageKind::Comment);
            api.apply(post).await.unwrap();
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
        api.apply(Change::DeleteNote { note: ids[0] })
            .await
            .unwrap();
        let edit = Change::EditNote {
            note: ids[1],
            text: "second, edited".into(),
        };
        api.apply(edit).await.unwrap();
        let notes = &api.board().task(a).unwrap().notes;
        assert_eq!(notes.len(), 1);
        assert_eq!(
            (notes[0].id, notes[0].text.as_str()),
            (ids[1], "second, edited")
        );
        assert_eq!(
            api.apply(Change::ToggleStar { note: ids[0] }).await,
            Err(ApiError::UnknownNote(ids[0]))
        );
    }

    #[tokio::test]
    async fn changes_round_trip_through_json() {
        let change = Change::post(
            Uuid::from_u128(3),
            "/block waiting".into(),
            MessageKind::Block,
        );
        let json = serde_json::to_string(&change).unwrap();
        assert_eq!(serde_json::from_str::<Change>(&json).unwrap(), change);
    }

    #[tokio::test]
    async fn taken_ids_are_refused() {
        let mut api = Api::new(Board::default());
        let a = add(&mut api, "a", false).await;
        let again = Change::AddTask {
            id: a,
            note_id: Uuid::new_v4(),
            title: "b".into(),
            notes: String::new(),
            parent: None,
            focus: false,
        };
        assert_eq!(api.apply(again).await, Err(ApiError::Duplicate(a)));
        let post = Change::post(a, "hi".into(), MessageKind::Comment);
        api.apply(post.clone()).await.unwrap();
        // The same post sent twice, say after a lost answer.
        let Change::Post { id, .. } = post else {
            unreachable!()
        };
        assert_eq!(api.apply(post).await, Err(ApiError::Duplicate(id)));
        assert_eq!(api.board().task(a).unwrap().notes.len(), 1);
    }

    #[test]
    fn a_change_gives_the_same_board_everywhere() {
        // What lets the GUI show a change before the server confirms it.
        let now = Utc::now();
        let (mut here, mut there) = (Board::default(), Board::default());
        let add = Change::add_task("a".into(), "first".into(), None, true);
        let Change::AddTask { id, .. } = add else {
            unreachable!()
        };
        let changes = [
            add,
            Change::post(id, "/next ship it".into(), MessageKind::Next),
            Change::Rename {
                task: id,
                title: "b".into(),
            },
        ];
        for change in changes {
            let a = apply_change(&mut here, change.clone(), now);
            let b = apply_change(&mut there, change, now);
            assert_eq!(a, b);
        }
        assert_eq!(here, there);
        let step = here.task(id).unwrap().notes[1].id;
        let promote = Change::promote_step(step, "ship".into(), String::new(), false);
        let a = apply_change(&mut here, promote.clone(), now);
        assert_eq!(a, apply_change(&mut there, promote, now));
        assert!(matches!(a, Ok(Outcome::Created(_))));
        assert_eq!(here, there);
    }
}
