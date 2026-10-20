//! The board a new user starts with: two tasks whose comments show how the
//! app works. They're ordinary tasks, to try things on and then delete.

use chrono::{DateTime, TimeDelta, Utc};

use crate::model::{Board, MessageKind};

pub fn board(now: DateTime<Utc>) -> Board {
    let ago = |minutes| now - TimeDelta::minutes(minutes);
    let mut b = Board::default();

    let welcome = b.add_task(
        "Welcome to its4tasks".into(),
        "Up to four tasks are in focus, as cards like this one\n\
         The rest wait in the list on the right. Press → there, or drag a task onto a \
         card, to bring it into focus; ✕ sends it back.",
        None,
        ago(5),
    );
    let t = b.task_mut(welcome).expect("just added");
    for (text, kind, at) in [
        (
            "Comments are the task's log\n\
             Type below and press Enter to post one. Shift+Enter starts a new line, and a \
             first line followed by more text becomes the comment's title.",
            MessageKind::Comment,
            ago(4),
        ),
        (
            "Commands do more than comment\n\
             Start a comment with /next for a next step, /block when you're waiting on \
             someone, /unblock when they've answered, /star to star it, /close to finish \
             the task, or /sub-task to make a subtask.",
            MessageKind::Star,
            ago(3),
        ),
        (
            "The Log view keeps the history\n\
             Switch to it at the top left to see what happened each day or week.",
            MessageKind::Comment,
            ago(2),
        ),
    ] {
        t.post(text, kind, at);
    }

    let try_it = b.add_task(
        "Try it out".into(),
        "Everything here is yours to change. Delete these tasks from their ⋯ menu when \
         you're done with them.",
        None,
        ago(1),
    );
    let t = b.task_mut(try_it).expect("just added");
    t.post(
        "Tick this step off with its checkbox",
        MessageKind::Next,
        now,
    );

    b.focus(welcome);
    b.focus(try_it);
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TaskState;

    #[test]
    fn welcomes_with_two_focused_tasks() {
        let b = board(Utc::now());
        assert_eq!(b.tasks.len(), 2);
        assert_eq!(b.slots[..2], [Some(b.tasks[0].id), Some(b.tasks[1].id)]);
        assert_eq!(b.state(b.tasks[1].id), TaskState::Ready);
        assert!(b.tasks[0].notes.iter().any(|n| n.starred));
    }
}
