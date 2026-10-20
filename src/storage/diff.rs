//! What changed between the last saved board and the current one.

use std::collections::{HashMap, HashSet};

use crate::model::{Board, SLOT_COUNT, Task};

#[derive(Debug, Default, PartialEq)]
pub struct Changes<'a> {
    /// New or changed tasks, parents before their subtasks so a row's parent
    /// is always written first. The flag says whether its notes changed.
    pub upserts: Vec<(&'a Task, bool)>,
    pub deletes: Vec<u64>,
    /// Slots whose task changed: `(index, task)`.
    pub slots: Vec<(usize, Option<u64>)>,
    pub next_id: Option<u64>,
}

impl Changes<'_> {
    pub fn is_empty(&self) -> bool {
        *self == Changes::default()
    }
}

pub fn diff<'a>(old: &Board, new: &'a Board) -> Changes<'a> {
    let before: HashMap<u64, &Task> = old.tasks.iter().map(|t| (t.id, t)).collect();
    let mut upserts: Vec<(&Task, bool)> = new
        .tasks
        .iter()
        .filter_map(|t| match before.get(&t.id) {
            None => Some((t, true)),
            Some(&o) if o != t => Some((t, o.notes != t.notes)),
            Some(_) => None,
        })
        .collect();
    upserts.sort_by_key(|(t, _)| depth(new, t));
    let kept: HashSet<u64> = new.tasks.iter().map(|t| t.id).collect();
    Changes {
        upserts,
        deletes: old
            .tasks
            .iter()
            .map(|t| t.id)
            .filter(|id| !kept.contains(id))
            .collect(),
        slots: (0..SLOT_COUNT)
            .filter(|&i| old.slots[i] != new.slots[i])
            .map(|i| (i, new.slots[i]))
            .collect(),
        next_id: (old.next_id != new.next_id).then_some(new.next_id),
    }
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

    fn ids(changes: &Changes) -> Vec<u64> {
        changes.upserts.iter().map(|(t, _)| t.id).collect()
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
        assert!(!c.upserts[0].1, "notes did not change");
        assert!(c.deletes.is_empty());
        assert_eq!(c.next_id, None);
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
        assert_eq!(changes.next_id, Some(3));
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
}
