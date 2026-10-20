//! Model values to table rows and back.

use sea_orm::{DbErr, IntoActiveModel};
use uuid::Uuid;

use super::entity::{note, task};
use crate::model::{Blocker, NextStep, Note, NoteAction, Task};

pub fn task_row(t: &Task) -> task::ActiveModel {
    task::Model {
        id: t.id as i64,
        title: t.title.clone(),
        done: t.done,
        updated_at: t.updated_at,
        created_at: t.created_at,
        done_at: t.done_at,
        parent_id: t.parent.map(|p| p as i64),
        folded: t.folded,
    }
    .into_active_model()
}

/// A task without its notes, which come from their own table.
pub fn task_from_row(row: task::Model) -> Task {
    Task {
        id: row.id as u64,
        title: row.title,
        notes: Vec::new(),
        done: row.done,
        updated_at: row.updated_at,
        created_at: row.created_at,
        done_at: row.done_at,
        parent: row.parent_id.map(|p| p as u64),
        folded: row.folded,
    }
}

pub fn note_row(task_id: u64, position: usize, n: &Note) -> note::ActiveModel {
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
            Some(subtask as i64),
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
        task_id: task_id as i64,
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
    let id = Uuid::parse_str(&row.id)
        .map_err(|e| DbErr::Custom(format!("note {}: bad id: {e}", row.id)))?;
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
                    subtask: required(row.next_step_subtask, "next_step_subtask", &row.id)? as u64,
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
