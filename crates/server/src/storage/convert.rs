//! Model values to table rows and back.

use sea_orm::{DbErr, IntoActiveModel};
use uuid::Uuid;

use super::entity::{note, task};
use its4tasks_core::model::{Blocker, NextStep, Note, NoteAction, Task};

pub fn task_row(owner: Uuid, t: &Task) -> task::ActiveModel {
    task::Model {
        id: t.id.to_string(),
        owner_id: owner.to_string(),
        title: t.title.clone(),
        done: t.done,
        updated_at: t.updated_at,
        created_at: t.created_at,
        done_at: t.done_at,
        parent_id: t.parent.map(|p| p.to_string()),
        folded: t.folded,
    }
    .into_active_model()
}

/// A task without its notes, which come from their own table.
pub fn task_from_row(row: task::Model) -> Result<Task, DbErr> {
    Ok(Task {
        id: parse_id(&row.id, "task")?,
        title: row.title,
        notes: Vec::new(),
        done: row.done,
        updated_at: row.updated_at,
        created_at: row.created_at,
        done_at: row.done_at,
        parent: row.parent_id.map(|p| parse_id(&p, "parent")).transpose()?,
        folded: row.folded,
    })
}

/// A UUID stored as text; `what` names it in the error.
pub fn parse_id(text: &str, what: &str) -> Result<Uuid, DbErr> {
    Uuid::parse_str(text).map_err(|e| DbErr::Custom(format!("bad {what} id {text:?}: {e}")))
}

pub fn note_row(owner: Uuid, task_id: Uuid, position: usize, n: &Note) -> note::ActiveModel {
    let (blocker_state, blocker_resolved_at) = match n.blocker {
        None => (None, None),
        Some(Blocker::Open) => (Some("open"), None),
        Some(Blocker::Resolved { at }) => (Some("resolved"), Some(at)),
    };
    let (next_step_state, next_step_since, next_step_at, next_step_subtask) = match n.next_step {
        None => (None, None, None, None),
        Some(NextStep::Open { since }) => (Some("open"), Some(since), None, None),
        Some(NextStep::Done { since, at }) => (Some("done"), Some(since), Some(at), None),
        Some(NextStep::Promoted { since, at, subtask }) => (
            Some("promoted"),
            Some(since),
            Some(at),
            Some(subtask.to_string()),
        ),
    };
    let (action_kind, action_quote, action_blocked_since) = match &n.action {
        None => (None, None, None),
        Some(NoteAction::Unblocked {
            quote,
            blocked_since,
        }) => (Some("unblocked"), Some(quote.clone()), Some(*blocked_since)),
        Some(NoteAction::Closed) => (Some("closed"), None, None),
    };
    note::Model {
        id: n.id.to_string(),
        owner_id: owner.to_string(),
        task_id: task_id.to_string(),
        position: position as i32,
        at: n.at,
        text: n.text.clone(),
        starred: n.starred,
        blocked_at: n.blocked_at,
        blocker_state: blocker_state.map(str::to_owned),
        blocker_resolved_at,
        next_step_state: next_step_state.map(str::to_owned),
        next_step_since,
        next_step_at,
        next_step_subtask,
        action_kind: action_kind.map(str::to_owned),
        action_quote,
        action_blocked_since,
    }
    .into_active_model()
}

pub fn note_from_row(row: note::Model) -> Result<Note, DbErr> {
    let id = parse_id(&row.id, "note")?;
    let blocker = match row.blocker_state.as_deref() {
        None => None,
        Some("open") => Some(Blocker::Open),
        Some("resolved") => Some(Blocker::Resolved {
            at: required(row.blocker_resolved_at, "blocker_resolved_at", &row.id)?,
        }),
        Some(other) => return Err(unknown("blocker_state", other, &row.id)),
    };
    let next_step = match row.next_step_state.as_deref() {
        None => None,
        Some(state) => {
            let since = required(row.next_step_since, "next_step_since", &row.id)?;
            Some(match state {
                "open" => NextStep::Open { since },
                "done" => NextStep::Done {
                    since,
                    at: required(row.next_step_at, "next_step_at", &row.id)?,
                },
                "promoted" => NextStep::Promoted {
                    since,
                    at: required(row.next_step_at, "next_step_at", &row.id)?,
                    subtask: parse_id(
                        &required(row.next_step_subtask, "next_step_subtask", &row.id)?,
                        "subtask",
                    )?,
                },
                other => return Err(unknown("next_step_state", other, &row.id)),
            })
        }
    };
    let action = match row.action_kind.as_deref() {
        None => None,
        Some("unblocked") => Some(NoteAction::Unblocked {
            quote: required(row.action_quote, "action_quote", &row.id)?,
            blocked_since: required(row.action_blocked_since, "action_blocked_since", &row.id)?,
        }),
        Some("closed") => Some(NoteAction::Closed),
        Some(other) => return Err(unknown("action_kind", other, &row.id)),
    };
    Ok(Note {
        id,
        at: row.at,
        text: row.text,
        blocker,
        blocked_at: row.blocked_at,
        starred: row.starred,
        action,
        next_step,
    })
}

fn required<T>(value: Option<T>, column: &str, note: &str) -> Result<T, DbErr> {
    value.ok_or_else(|| DbErr::Custom(format!("note {note}: {column} is missing")))
}

fn unknown(column: &str, value: &str, note: &str) -> DbErr {
    DbErr::Custom(format!("note {note}: unknown {column} {value:?}"))
}
