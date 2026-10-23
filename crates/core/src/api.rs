//! The only way to change the board: a client reads a `Board` and sends
//! `Change`s, which travel over HTTP as JSON (`GET /api/board`,
//! `POST /api/changes`, answered with `Applied` or `Rejected`).
//!
//! `apply_change` holds the rules. The server applies a change with it, and
//! the GUI applies the same change to its own copy straight away. For both
//! to agree, changes that create a task or comment carry the new ids, picked
//! by the client.

use std::collections::HashSet;
use std::fmt;

#[cfg(test)]
use chrono::TimeDelta;
use chrono::{DateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::{Board, MessageKind, MoveError, Note, NoteAction, Task};

/// The longest task title, in bytes.
pub const MAX_TITLE: usize = 2 << 10;
/// The longest comment, in bytes.
pub const MAX_TEXT: usize = 64 << 10;
/// The most tasks a board holds.
pub const MAX_TASKS: usize = 20_000;
/// The most comments a board holds, over all its tasks.
pub const MAX_NOTES: usize = 200_000;

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
    /// Adds another board's tasks: a new user's welcome board, or an
    /// uploaded `its4tasks.json`. Its ids must be new (see `import::fresh_ids`).
    /// Tasks it had in focus are focused where there's room.
    Import {
        board: Board,
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
    /// The task or comment is on someone else's board.
    Forbidden(Uuid),
    /// The board's rules don't allow it, e.g. a parent that makes a cycle.
    Refused,
    /// A title or comment is too long, or the board would get too big (see
    /// `MAX_TITLE` and the like).
    TooBig,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ApiError::UnknownTask(id) => write!(f, "There is no task {id}"),
            ApiError::UnknownNote(id) => write!(f, "There is no comment {id}"),
            ApiError::Move(e) => e.fmt(f),
            ApiError::Duplicate(id) => write!(f, "The id {id} is taken"),
            ApiError::Forbidden(id) => write!(f, "{id} is on someone else's board"),
            ApiError::Refused => f.write_str("Not allowed"),
            ApiError::TooBig => f.write_str("That's too big for a board"),
        }
    }
}

/// The time to apply changes at: now, to the microsecond, which is as
/// precise as every database keeps it (Postgres drops nanoseconds), so a
/// board saved and loaded again is the same board.
pub fn now() -> DateTime<Utc> {
    let now = Utc::now();
    now.with_nanosecond(now.nanosecond() / 1000 * 1000)
        .unwrap_or(now)
}

/// A user's board as `GET /api/board` answers it.
///
/// Here and in `Applied` and `Rejected`, the board leaves out archived
/// tasks (see `Board::archived`) unless the request asks for them with
/// `?archived=true`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardView {
    /// False for a new user, until they start a board or import one.
    pub started: bool,
    pub board: Board,
    /// Whose board it is, as the server knows them: so work kept in the
    /// browser goes back to the same user.
    #[serde(default)]
    pub user: Option<Uuid>,
}

/// What a change did, and the board after it.
#[derive(Debug, Serialize, Deserialize)]
pub struct Applied {
    pub outcome: Outcome,
    pub board: Board,
}

/// Why a change was turned down, and the board it was turned down by:
/// often the client's copy was out of date.
#[derive(Debug, Serialize, Deserialize)]
pub struct Rejected {
    pub error: ApiError,
    pub board: Board,
}

/// Applies a change to a board as of `now`: the board's rules, shared by the
/// server and the GUI's optimistic copy. The same change on equal boards at
/// the same `now` gives equal boards.
///
/// A change that updates a subtask updates the tasks above it too.
pub fn apply_change(
    b: &mut Board,
    change: Change,
    now: DateTime<Utc>,
) -> Result<Outcome, ApiError> {
    let subject = subject(b, &change);
    let updated = |b: &Board| subject.and_then(|id| b.task(id)).map(|t| t.updated_at);
    let before = updated(b);
    let outcome = apply(b, change, now)?;
    if let (Some(id), Some(at)) = (subject, updated(b))
        && Some(at) != before
    {
        b.touch_ancestors(id, at);
    }
    Ok(outcome)
}

/// The task whose update a change can be: the one it changes, or the one it
/// creates.
fn subject(b: &Board, change: &Change) -> Option<Uuid> {
    match change {
        Change::AddTask { id, .. } => Some(*id),
        Change::Rename { task, .. }
        | Change::SetDone { task, .. }
        | Change::ToggleFold { task }
        | Change::SetParent { task, .. }
        | Change::Post { task, .. } => Some(*task),
        Change::MoveNote { to, .. } => Some(*to),
        Change::PromoteStep { note, .. }
        | Change::EditNote { note, .. }
        | Change::DeleteNote { note }
        | Change::MarkBlocking { note }
        | Change::Resolve { note }
        | Change::Unblock { note }
        | Change::MarkStep { note }
        | Change::ToggleStep { note }
        | Change::UnmarkStep { note }
        | Change::ToggleStar { note } => b.find_note(*note).map(|(task, _)| task),
        Change::DeleteTask { .. }
        | Change::Focus { .. }
        | Change::FocusInto { .. }
        | Change::Unfocus { .. }
        | Change::Import { .. } => None,
    }
}

fn apply(b: &mut Board, change: Change, now: DateTime<Utc>) -> Result<Outcome, ApiError> {
    fits(b, &change)?;
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
        Change::Import { board: imported } => import(b, imported)?,
        Change::MoveNote { note: id, to } => {
            let (from, idx) = find(b, id)?;
            b.move_note(from, idx, to).map_err(ApiError::Move)?;
        }
    }
    Ok(Outcome::Done)
}

/// Whether the board still fits the limits after `change`, as far as its
/// size goes.
fn fits(b: &Board, change: &Change) -> Result<(), ApiError> {
    let long_title = |title: &str| title.len() > MAX_TITLE;
    let long_text = |text: &str| text.len() > MAX_TEXT;
    let long_note = |n: &Note| {
        long_text(&n.text)
            || matches!(&n.action, Some(NoteAction::Unblocked { quote, .. }) if long_text(quote))
    };
    let (too_long, new_tasks, new_notes) = match change {
        Change::AddTask { title, notes, .. } | Change::PromoteStep { title, notes, .. } => {
            (long_title(title) || long_text(notes), 1, 1)
        }
        Change::Rename { title, .. } => (long_title(title), 0, 0),
        Change::Post { text, .. } => (long_text(text), 0, 1),
        Change::EditNote { text, .. } => (long_text(text), 0, 0),
        Change::Import { board } => (
            board
                .tasks
                .iter()
                .any(|t| long_title(&t.title) || t.notes.iter().any(long_note)),
            board.tasks.len(),
            board.tasks.iter().map(|t| t.notes.len()).sum(),
        ),
        _ => (false, 0, 0),
    };
    let notes = || b.tasks.iter().map(|t| t.notes.len()).sum::<usize>();
    if too_long
        || (new_tasks > 0 && b.tasks.len() + new_tasks > MAX_TASKS)
        || (new_notes > 0 && notes() + new_notes > MAX_NOTES)
    {
        return Err(ApiError::TooBig);
    }
    Ok(())
}

fn import(b: &mut Board, imported: Board) -> Result<(), ApiError> {
    let mut tasks = HashSet::new();
    let mut notes = HashSet::new();
    for t in &imported.tasks {
        unused_task(b, t.id)?;
        if !tasks.insert(t.id) {
            return Err(ApiError::Duplicate(t.id));
        }
        for n in &t.notes {
            unused_note(b, n.id)?;
            if !notes.insert(n.id) {
                return Err(ApiError::Duplicate(n.id));
            }
        }
    }
    for mut t in imported.tasks {
        // A parent outside the imported tasks is gone.
        t.parent = t.parent.filter(|p| tasks.contains(p));
        b.tasks.push(t);
    }
    // A parent making a cycle (say, in a hand-edited file) is dropped.
    for &id in &tasks {
        if let Some(p) = b.task(id).and_then(|t| t.parent)
            && b.is_descendant(p, id)
            && let Some(t) = b.task_mut(id)
        {
            t.parent = None;
        }
    }
    for &id in imported.slots.iter().flatten() {
        if tasks.contains(&id) {
            b.focus(id);
        }
    }
    Ok(())
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
    use super::*;

    #[test]
    fn changes_round_trip_through_json() {
        let change = Change::post(
            Uuid::from_u128(3),
            "/block waiting".into(),
            MessageKind::Block,
        );
        let json = serde_json::to_string(&change).unwrap();
        assert_eq!(serde_json::from_str::<Change>(&json).unwrap(), change);
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

    #[test]
    fn updating_a_subtask_updates_the_tasks_above_it() {
        let t0 = now();
        let h = TimeDelta::hours;
        let mut b = Board::default();
        let top = b.add_task("top".into(), "", None, t0);
        let mid = b.add_task("mid".into(), "", Some(top), t0);
        let at = |b: &Board, id| b.task(id).unwrap().updated_at;
        let apply = |b: &mut Board, change, now| apply_change(b, change, now).unwrap();

        // A new subtask counts.
        let Outcome::Created(low) = apply(
            &mut b,
            Change::add_task("low".into(), String::new(), Some(mid), false),
            t0 + h(1),
        ) else {
            panic!("created")
        };
        assert_eq!((at(&b, top), at(&b, mid)), (t0 + h(1), t0 + h(1)));

        let post = Change::post(low, "/next ship".into(), MessageKind::Next);
        apply(&mut b, post, t0 + h(2));
        assert_eq!((at(&b, top), at(&b, mid)), (t0 + h(2), t0 + h(2)));
        let step = b.task(low).unwrap().notes[0].id;
        apply(&mut b, Change::ToggleStep { note: step }, t0 + h(3));
        assert_eq!(at(&b, top), t0 + h(3));
        let rename = Change::Rename {
            task: low,
            title: "lower".into(),
        };
        apply(&mut b, rename, t0 + h(4));
        assert_eq!(at(&b, top), t0 + h(4));

        // What doesn't update the subtask doesn't update its parents.
        apply(&mut b, Change::ToggleStar { note: step }, t0 + h(5));
        apply(&mut b, Change::ToggleFold { task: low }, t0 + h(5));
        assert_eq!((at(&b, top), at(&b, low)), (t0 + h(4), t0 + h(4)));

        // A moved comment updates them only as of its own time.
        let other = b.add_task("other".into(), "", None, t0);
        b.task_mut(other).unwrap().post(
            "old",
            MessageKind::Comment,
            t0 + h(4) + TimeDelta::minutes(30),
        );
        let note = b.task(other).unwrap().notes[0].id;
        apply(&mut b, Change::MoveNote { note, to: low }, t0 + h(6));
        assert_eq!(at(&b, top), t0 + h(4) + TimeDelta::minutes(30));
    }

    #[test]
    fn boards_have_a_size_limit() {
        let now = now();
        let mut b = Board::default();
        let long = |n: usize| "x".repeat(n);
        let add = |title: String, notes: String| Change::add_task(title, notes, None, false);
        assert_eq!(
            apply_change(&mut b, add(long(MAX_TITLE + 1), String::new()), now),
            Err(ApiError::TooBig)
        );
        assert_eq!(
            apply_change(&mut b, add("a".into(), long(MAX_TEXT + 1)), now),
            Err(ApiError::TooBig)
        );
        assert!(b.tasks.is_empty(), "nothing added");
        let Ok(Outcome::Created(id)) =
            apply_change(&mut b, add(long(MAX_TITLE), long(MAX_TEXT)), now)
        else {
            panic!("as long as allowed is fine")
        };
        let post = Change::post(id, long(MAX_TEXT + 1), MessageKind::Comment);
        assert_eq!(apply_change(&mut b, post, now), Err(ApiError::TooBig));
        let note = b.tasks[0].notes[0].id;
        let edit = Change::EditNote {
            note,
            text: long(MAX_TEXT + 1),
        };
        assert_eq!(apply_change(&mut b, edit, now), Err(ApiError::TooBig));
        let rename = Change::Rename {
            task: id,
            title: long(MAX_TITLE + 1),
        };
        assert_eq!(apply_change(&mut b, rename, now), Err(ApiError::TooBig));

        // Too many tasks, all at once.
        let mut big = Board::default();
        for _ in 0..MAX_TASKS {
            big.add_task("t".into(), "", None, now);
        }
        let import = Change::Import {
            board: crate::import::fresh_ids(big),
        };
        assert_eq!(apply_change(&mut b, import, now), Err(ApiError::TooBig));
        assert_eq!(b.tasks.len(), 1);
    }

    #[test]
    fn imports_add_tasks_with_their_shape() {
        let now = now();
        let mut b = Board::default();
        let mine = b.add_task("mine".into(), "", None, now);
        b.focus(mine);
        let mut other = crate::welcome::board(now);
        let sub = other.add_task("sub".into(), "", Some(other.tasks[0].id), now);
        let other = crate::import::fresh_ids(other);
        let (first, second) = (other.tasks[0].id, other.tasks[1].id);
        let import = Change::Import {
            board: other.clone(),
        };
        assert_eq!(apply_change(&mut b, import.clone(), now), Ok(Outcome::Done));
        assert_eq!(b.tasks.len(), 4);
        assert_eq!(b.tasks[3].parent, Some(first));
        assert_ne!(b.tasks[3].id, sub, "fresh ids");
        // Focused where there was room, after what was already in focus.
        assert_eq!(b.slots[..3], [Some(mine), Some(first), Some(second)]);
        // The same board twice clashes.
        assert_eq!(
            apply_change(&mut b, import, now),
            Err(ApiError::Duplicate(first))
        );
    }

    #[test]
    fn imports_drop_parent_cycles_and_outside_parents() {
        let now = now();
        let mut other = Board::default();
        let a = other.add_task("a".into(), "", None, now);
        let c = other.add_task("c".into(), "", Some(a), now);
        other.task_mut(a).unwrap().parent = Some(c);
        let outside = other.add_task("d".into(), "", None, now);
        other.task_mut(outside).unwrap().parent = Some(Uuid::new_v4());
        let mut b = Board::default();
        let import = Change::Import { board: other };
        apply_change(&mut b, import, now).unwrap();
        assert!(b.task(outside).unwrap().parent.is_none());
        let parents = [b.task(a).unwrap().parent, b.task(c).unwrap().parent];
        assert!(parents.contains(&None), "the cycle is broken");
    }
}
