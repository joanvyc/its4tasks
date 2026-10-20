//! What changed between the last saved board and the current one.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use its4tasks_core::model::{Board, Note, SLOT_COUNT, Task};

#[derive(Debug, Default, PartialEq)]
pub struct Changes<'a> {
    /// New or changed task rows (notes aside), parents before their subtasks
    /// so a row's parent is always written first.
    pub upserts: Vec<&'a Task>,
    pub deletes: Vec<Uuid>,
    /// New or changed notes, with their task and position in it. A moved
    /// note is here with its new task.
    pub note_upserts: Vec<(Uuid, usize, &'a Note)>,
    /// Notes gone from tasks that remain. Those of deleted tasks go with them.
    pub note_deletes: Vec<Uuid>,
    /// Slots whose task changed: `(index, task)`.
    pub slots: Vec<(usize, Option<Uuid>)>,
}

impl Changes<'_> {
    pub fn is_empty(&self) -> bool {
        *self == Changes::default()
    }
}

pub fn diff<'a>(old: &Board, new: &'a Board) -> Changes<'a> {
    let before: HashMap<Uuid, &Task> = old.tasks.iter().map(|t| (t.id, t)).collect();
    // New tasks, or ones that changed in any way, notes included.
    let changed: Vec<(Option<&Task>, &Task)> = new
        .tasks
        .iter()
        .map(|t| (before.get(&t.id).copied(), t))
        .filter(|(o, t)| *o != Some(*t))
        .collect();

    let mut upserts: Vec<&Task> = changed
        .iter()
        .filter(|(o, t)| !o.is_some_and(|o| same_row(o, t)))
        .map(|(_, t)| *t)
        .collect();
    upserts.sort_by_key(|t| depth(new, t));

    let notes_before: HashMap<Uuid, (Uuid, usize, &Note)> = changed
        .iter()
        .filter_map(|(o, _)| *o)
        .flat_map(|o| {
            o.notes
                .iter()
                .enumerate()
                .map(|(i, n)| (n.id, (o.id, i, n)))
        })
        .collect();
    let note_upserts: Vec<(Uuid, usize, &Note)> = changed
        .iter()
        .flat_map(|(_, t)| t.notes.iter().enumerate().map(|(i, n)| (t.id, i, n)))
        .filter(|row| notes_before.get(&row.2.id) != Some(row))
        .collect();
    let notes_after: HashSet<Uuid> = changed
        .iter()
        .flat_map(|(_, t)| t.notes.iter().map(|n| n.id))
        .collect();
    let note_deletes = notes_before
        .keys()
        .filter(|id| !notes_after.contains(id))
        .copied()
        .collect();

    let kept: HashSet<Uuid> = new.tasks.iter().map(|t| t.id).collect();
    Changes {
        upserts,
        deletes: old
            .tasks
            .iter()
            .map(|t| t.id)
            .filter(|id| !kept.contains(id))
            .collect(),
        note_upserts,
        note_deletes,
        slots: (0..SLOT_COUNT)
            .filter(|&i| old.slots[i] != new.slots[i])
            .map(|i| (i, new.slots[i]))
            .collect(),
    }
}

/// Whether two versions of a task have the same row, ignoring their notes.
fn same_row(a: &Task, b: &Task) -> bool {
    // Destructured so a new field can't be forgotten here.
    let Task {
        id,
        title,
        notes: _,
        done,
        updated_at,
        created_at,
        done_at,
        parent,
        folded,
    } = a;
    (
        id, title, done, updated_at, created_at, done_at, parent, folded,
    ) == (
        &b.id,
        &b.title,
        &b.done,
        &b.updated_at,
        &b.created_at,
        &b.done_at,
        &b.parent,
        &b.folded,
    )
}

/// How many ancestors a task has. Stops at the task count in case of a cycle.
fn depth(board: &Board, task: &Task) -> usize {
    let mut depth = 0;
    let mut parent = task.parent;
    while let Some(p) = parent
        && depth < board.tasks.len()
    {
        depth += 1;
        parent = board.task(p).and_then(|t| t.parent);
    }
    depth
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use its4tasks_core::model::MessageKind;

    fn ids(changes: &Changes) -> Vec<Uuid> {
        changes.upserts.iter().map(|t| t.id).collect()
    }

    #[test]
    fn unchanged_board_has_no_changes() {
        let mut b = Board::default();
        b.add_task("a".into(), "note", None, Utc::now());
        assert!(diff(&b, &b.clone()).is_empty());
    }

    #[test]
    fn lists_only_changed_tasks() {
        let now = Utc::now();
        let mut old = Board::default();
        let a = old.add_task("a".into(), "", None, now);
        old.add_task("b".into(), "", None, now);
        let mut new = old.clone();
        new.task_mut(a).unwrap().title = "renamed".into();
        let c = diff(&old, &new);
        assert_eq!(ids(&c), vec![a]);
        assert!(c.note_upserts.is_empty(), "notes did not change");
        assert!(c.deletes.is_empty());
    }

    #[test]
    fn parents_come_before_subtasks() {
        let now = Utc::now();
        let old = Board::default();
        let mut new = Board::default();
        let a = new.add_task("a".into(), "", None, now);
        let b = new.add_task("b".into(), "", Some(a), now);
        let c = new.add_task("c".into(), "", Some(b), now);
        // List order no longer matches the hierarchy.
        new.tasks.reverse();
        let changes = diff(&old, &new);
        assert_eq!(ids(&changes), vec![a, b, c]);
    }

    #[test]
    fn deleted_tasks_and_cleared_slots() {
        let now = Utc::now();
        let mut old = Board::default();
        let a = old.add_task("a".into(), "", None, now);
        let b = old.add_task("b".into(), "", Some(a), now);
        old.slots[2] = Some(b);
        let mut new = old.clone();
        new.delete_task(a);
        let c = diff(&old, &new);
        assert!(c.upserts.is_empty());
        assert_eq!(c.deletes, vec![a, b]);
        assert_eq!(c.slots, vec![(2, None)]);
    }

    #[test]
    fn writes_only_the_notes_that_changed() {
        let now = Utc::now();
        let mut old = Board::default();
        let a = old.add_task("a".into(), "one", None, now);
        let b = old.add_task("b".into(), "", None, now);
        let t = old.task_mut(a).unwrap();
        t.post("two", MessageKind::Comment, now);
        t.post("three", MessageKind::Comment, now);
        let ids: Vec<Uuid> = t.notes.iter().map(|n| n.id).collect();

        let mut new = old.clone();
        new.task_mut(a).unwrap().notes[2].text = "three!".into();
        let c = diff(&old, &new);
        assert!(c.upserts.is_empty(), "the task row didn't change");
        let rows: Vec<_> = c
            .note_upserts
            .iter()
            .map(|(t, i, n)| (*t, *i, n.id))
            .collect();
        assert_eq!(rows, vec![(a, 2, ids[2])]);
        assert!(c.note_deletes.is_empty());

        // Deleting the first shifts the rest; moving keeps the id.
        let mut new = old.clone();
        new.task_mut(a).unwrap().delete_note(0);
        new.move_note(a, 1, b).unwrap();
        let c = diff(&old, &new);
        let rows: Vec<_> = c
            .note_upserts
            .iter()
            .map(|(t, i, n)| (*t, *i, n.id))
            .collect();
        assert_eq!(rows, vec![(a, 0, ids[1]), (b, 0, ids[2])]);
        assert_eq!(c.note_deletes, vec![ids[0]]);
    }
}
