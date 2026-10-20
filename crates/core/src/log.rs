//! Daily/weekly history of what happened to each task, built from the board.

use chrono::{DateTime, Datelike, NaiveDate, TimeDelta, TimeZone, Utc};
use uuid::Uuid;

use crate::model::{Blocker, Board, NextStep, Note, NoteAction, Task, headline};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Day,
    Week,
}

impl Period {
    pub fn label(self) -> &'static str {
        match self {
            Period::Day => "Daily",
            Period::Week => "Weekly",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EntryKind {
    Created,
    Comment {
        blocking: bool,
        next_step: bool,
    },
    /// An existing comment turned into a blocker.
    MarkedBlocking,
    /// An existing comment turned into a next step.
    MarkedNextStep,
    /// A next step ticked off.
    StepDone,
    /// A next step turned into this subtask (its title as of now).
    Promoted {
        title: String,
    },
    Unblocked {
        after: TimeDelta,
    },
    /// Posted with "Unblock": resolved the quoted blocker; the text is the message.
    Resolved {
        quote: String,
        after: TimeDelta,
    },
    Done,
    /// Posted with "Close": marked the task done; the text is the message.
    Closed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub at: DateTime<Utc>,
    pub kind: EntryKind,
    pub text: String,
    pub starred: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskGroup {
    pub task_id: Uuid,
    pub title: String,
    /// Empty for a parent that only holds its subtasks' entries on this page.
    pub entries: Vec<Entry>,
    pub subgroups: Vec<TaskGroup>,
}

impl TaskGroup {
    fn new(task: &Task) -> Self {
        Self {
            task_id: task.id,
            title: task.title.clone(),
            entries: Vec::new(),
            subgroups: Vec::new(),
        }
    }

    /// The earliest entry here or in any subgroup.
    fn first_at(&self) -> Option<DateTime<Utc>> {
        let own = self.entries.first().map(|e| e.at);
        own.into_iter()
            .chain(self.subgroups.iter().filter_map(TaskGroup::first_at))
            .min()
    }

    /// Entries in time order, subgroups by their earliest entry, all the way down.
    fn sort(&mut self) {
        self.entries.sort_by_key(|e| e.at);
        for group in &mut self.subgroups {
            group.sort();
        }
        self.subgroups.sort_by_key(TaskGroup::first_at);
    }
}

/// Put each flat group under its parent's group, adding (empty) groups for
/// parents with nothing of their own on the page.
fn nest(board: &Board, flat: Vec<TaskGroup>) -> Vec<TaskGroup> {
    let mut groups = flat;
    loop {
        let missing: Vec<Uuid> = groups
            .iter()
            .filter_map(|g| board.task(g.task_id)?.parent)
            .filter(|p| board.task(*p).is_some() && !groups.iter().any(|g| g.task_id == *p))
            .collect();
        if missing.is_empty() {
            break;
        }
        for id in missing {
            if !groups.iter().any(|g| g.task_id == id)
                && let Some(task) = board.task(id)
            {
                groups.push(TaskGroup::new(task));
            }
        }
    }
    let parent_of = |g: &TaskGroup| board.task(g.task_id).and_then(|t| t.parent);
    fn take_children(
        id: Option<Uuid>,
        groups: &mut Vec<TaskGroup>,
        parent_of: &dyn Fn(&TaskGroup) -> Option<Uuid>,
    ) -> Vec<TaskGroup> {
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(groups)
            .into_iter()
            .partition(|g| parent_of(g) == id);
        *groups = rest;
        mine.into_iter()
            .map(|mut g| {
                g.subgroups = take_children(Some(g.task_id), groups, parent_of);
                g
            })
            .collect()
    }
    take_children(None, &mut groups, &parent_of)
}

/// One day, or one Monday-to-Sunday week.
#[derive(Debug, Clone, PartialEq)]
pub struct LogPage {
    pub start: NaiveDate,
    pub groups: Vec<TaskGroup>,
}

fn note_entries(board: &Board, note: &Note) -> Vec<Entry> {
    let entry = |at, kind| Entry {
        at,
        kind,
        text: note.text.clone(),
        starred: note.starred,
    };
    match (&note.action, note.unblocked_after()) {
        (Some(NoteAction::Unblocked { quote, .. }), Some(after)) => {
            // Only a multi-line blocker's title is quoted.
            let quote = headline(quote).to_owned();
            return vec![entry(note.at, EntryKind::Resolved { quote, after })];
        }
        (Some(NoteAction::Closed), _) => return vec![entry(note.at, EntryKind::Closed)],
        _ => {}
    }
    let posted_blocking = note.blocker.is_some() && note.blocked_since() == note.at;
    let posted_step = note.next_step.is_some_and(|s| s.since() == note.at);
    let mut entries = vec![entry(
        note.at,
        EntryKind::Comment {
            blocking: posted_blocking,
            next_step: posted_step,
        },
    )];
    if note.blocker.is_some() && !posted_blocking {
        entries.push(entry(note.blocked_since(), EntryKind::MarkedBlocking));
    }
    if let Some(step) = note.next_step
        && !posted_step
    {
        entries.push(entry(step.since(), EntryKind::MarkedNextStep));
    }
    match note.next_step {
        Some(NextStep::Done { at, .. }) => entries.push(entry(at, EntryKind::StepDone)),
        Some(NextStep::Promoted { at, subtask, .. }) => {
            let title = match board.task(subtask) {
                Some(t) => t.title.clone(),
                None => headline(&note.text).to_owned(),
            };
            // The title says it all; the text lives on in the subtask.
            entries.push(Entry {
                text: String::new(),
                ..entry(at, EntryKind::Promoted { title })
            });
        }
        _ => {}
    }
    if let (Some(Blocker::Resolved { at }), Some(after)) = (note.blocker, note.blocked_duration()) {
        entries.push(entry(at, EntryKind::Unblocked { after }));
    }
    entries
}

fn page_start<Tz: TimeZone>(at: DateTime<Utc>, period: Period, tz: &Tz) -> NaiveDate {
    let day = at.with_timezone(tz).date_naive();
    match period {
        Period::Day => day,
        Period::Week => day - TimeDelta::days(day.weekday().num_days_from_monday().into()),
    }
}

/// Pages newest first; tasks within a page ordered by their first entry,
/// subtasks nested under their parent, entries in time order. `starred_only` keeps only starred comments' entries.
pub fn build<Tz: TimeZone>(
    board: &Board,
    period: Period,
    starred_only: bool,
    tz: &Tz,
) -> Vec<LogPage> {
    let mut pages: Vec<LogPage> = Vec::new();
    for task in &board.tasks {
        let title_entry = |at, kind| Entry {
            at,
            kind,
            text: String::new(),
            starred: false,
        };
        // Pushed in the order they should appear on equal timestamps (the sort
        // below is stable): Created, then comments, then Done.
        let mut entries: Vec<Entry> = task
            .created_at
            .map(|at| title_entry(at, EntryKind::Created))
            .into_iter()
            .collect();
        entries.extend(task.notes.iter().flat_map(|n| note_entries(board, n)));
        entries.extend(task.done_at.map(|at| title_entry(at, EntryKind::Done)));
        // An Unblock or Close message already says what its plain Unblocked or
        // Done entry would.
        let times = |f: fn(&EntryKind) -> bool| -> Vec<DateTime<Utc>> {
            entries
                .iter()
                .filter(|e| f(&e.kind))
                .map(|e| e.at)
                .collect()
        };
        let resolved = times(|k| matches!(k, EntryKind::Resolved { .. }));
        let closed = times(|k| matches!(k, EntryKind::Closed));
        entries.retain(|e| match e.kind {
            EntryKind::Unblocked { .. } => !resolved.contains(&e.at),
            EntryKind::Done => !closed.contains(&e.at),
            _ => true,
        });
        if starred_only {
            entries.retain(|e| e.starred);
        }
        for entry in entries {
            let start = page_start(entry.at, period, tz);
            let page = match pages.iter().position(|p| p.start == start) {
                Some(i) => &mut pages[i],
                None => {
                    pages.push(LogPage {
                        start,
                        groups: Vec::new(),
                    });
                    pages.last_mut().unwrap()
                }
            };
            match page.groups.iter_mut().find(|g| g.task_id == task.id) {
                Some(group) => group.entries.push(entry),
                None => {
                    let mut group = TaskGroup::new(task);
                    group.entries.push(entry);
                    page.groups.push(group);
                }
            }
        }
    }
    for page in &mut pages {
        page.groups = nest(board, std::mem::take(&mut page.groups));
        for group in &mut page.groups {
            group.sort();
        }
        page.groups.sort_by_key(TaskGroup::first_at);
    }
    pages.sort_by_key(|p| std::cmp::Reverse(p.start));
    pages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MessageKind;

    fn at(day: u32, hour: u32) -> DateTime<Utc> {
        // 2026-10-12 is a Monday.
        Utc.with_ymd_and_hms(2026, 10, day, hour, 0, 0).unwrap()
    }

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, day).unwrap()
    }

    /// Task 0: created on the 12th, a blocker on the 13th resolved on the 14th,
    /// a plain comment on the 14th, done on the 19th (next week).
    /// Task 1: a comment on the 14th at 08:00, later marked blocking on the 15th.
    fn sample() -> Board {
        let mut b = Board::default();
        let a = b.add_task("A".into(), "", None, at(12, 9));
        let t = b.task_mut(a).unwrap();
        t.post("waiting on input", MessageKind::Block, at(13, 10));
        t.resolve(0, at(14, 13));
        t.post("got it", MessageKind::Comment, at(14, 14));
        t.set_done(true, at(19, 9));
        let c = b.add_task("B".into(), "early note", None, at(14, 8));
        let t = b.task_mut(c).unwrap();
        t.set_blocking(0, at(15, 11));
        b
    }

    #[test]
    fn daily_pages_newest_first_with_tasks_in_order() {
        let pages = build(&sample(), Period::Day, false, &Utc);
        let starts: Vec<_> = pages.iter().map(|p| p.start).collect();
        assert_eq!(starts, [date(19), date(15), date(14), date(13), date(12)]);

        let day14 = &pages[2];
        let titles: Vec<_> = day14.groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["B", "A"]); // B's 08:00 comment comes before A's 13:00 unblock
        let kinds: Vec<_> = day14.groups[1].entries.iter().map(|e| &e.kind).collect();
        assert_eq!(
            kinds,
            [
                &EntryKind::Unblocked {
                    after: TimeDelta::hours(27)
                },
                &EntryKind::Comment {
                    blocking: false,
                    next_step: false
                },
            ]
        );
        assert_eq!(pages[0].groups[0].entries[0].kind, EntryKind::Done);
        assert_eq!(pages[4].groups[0].entries[0].kind, EntryKind::Created);
    }

    #[test]
    fn blockers_posted_vs_marked_later() {
        let pages = build(&sample(), Period::Day, false, &Utc);
        let posted = &pages[3].groups[0].entries[0];
        assert_eq!(
            posted.kind,
            EntryKind::Comment {
                blocking: true,
                next_step: false
            }
        );
        let marked = &pages[1].groups[0].entries[0];
        assert_eq!(marked.kind, EntryKind::MarkedBlocking);
        assert_eq!(marked.text, "early note");
    }

    #[test]
    fn next_steps_posted_marked_done_and_promoted() {
        let mut b = Board::default();
        let a = b.add_task("A".into(), "", None, at(12, 9));
        let t = b.task_mut(a).unwrap();
        t.post("call Bob", MessageKind::Next, at(12, 10));
        t.toggle_step(0, at(12, 11));
        t.post("Report\nwith numbers", MessageKind::Comment, at(12, 12));
        t.mark_next_step(1, at(12, 13));
        b.promote_step(a, 1, "Report".into(), "with numbers", at(12, 14));
        let pages = build(&b, Period::Day, false, &Utc);
        let group = &pages[0].groups[0];
        let kinds: Vec<_> = group.entries.iter().map(|e| &e.kind).collect();
        assert_eq!(
            kinds,
            [
                &EntryKind::Created,
                &EntryKind::Comment {
                    blocking: false,
                    next_step: true
                },
                &EntryKind::StepDone,
                &EntryKind::Comment {
                    blocking: false,
                    next_step: false
                },
                &EntryKind::MarkedNextStep,
                &EntryKind::Promoted {
                    title: "Report".into()
                },
            ]
        );
        // The subtask's history starts when the step was posted.
        let sub = &group.subgroups[0];
        assert_eq!(sub.entries[0].kind, EntryKind::Created);
        assert_eq!(sub.entries[0].at, at(12, 12));
    }

    #[test]
    fn created_comes_before_a_first_comment_at_the_same_time() {
        let pages = build(&sample(), Period::Day, false, &Utc);
        let b = pages[2].groups.iter().find(|g| g.title == "B").unwrap();
        let kinds: Vec<_> = b.entries.iter().map(|e| &e.kind).collect();
        assert_eq!(
            kinds,
            [
                &EntryKind::Created,
                &EntryKind::Comment {
                    blocking: false,
                    next_step: false
                }
            ]
        );
    }

    #[test]
    fn weekly_pages_split_on_monday() {
        let pages = build(&sample(), Period::Week, false, &Utc);
        let starts: Vec<_> = pages.iter().map(|p| p.start).collect();
        assert_eq!(starts, [date(19), date(12)]);
        let titles: Vec<_> = pages[1].groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["A", "B"]);
    }

    #[test]
    fn starred_only_keeps_starred_comments_and_their_unblocks() {
        let pages = build(&sample(), Period::Day, true, &Utc);
        let all: Vec<_> = pages
            .iter()
            .flat_map(|p| p.groups.iter().flat_map(|g| &g.entries))
            .collect();
        // Both blockers were auto-starred; the plain comment, Created and Done drop out.
        assert!(all.iter().all(|e| e.starred));
        assert_eq!(all.len(), 4);
        assert!(
            all.iter()
                .any(|e| matches!(e.kind, EntryKind::Unblocked { .. }))
        );
    }

    #[test]
    fn unblock_and_close_messages_replace_their_plain_entries() {
        let mut b = Board::default();
        let id = b.add_task("C".into(), "", None, at(12, 9));
        let t = b.task_mut(id).unwrap();
        t.post(
            "need access\nto the prod db",
            MessageKind::Block,
            at(12, 10),
        );
        t.post("got access", MessageKind::Unblock, at(12, 15));
        t.post("shipped", MessageKind::Close, at(12, 16));
        let pages = build(&b, Period::Day, false, &Utc);
        let kinds: Vec<_> = pages[0].groups[0].entries.iter().map(|e| &e.kind).collect();
        assert_eq!(
            kinds,
            [
                &EntryKind::Created,
                &EntryKind::Comment {
                    blocking: true,
                    next_step: false
                },
                &EntryKind::Resolved {
                    quote: "need access".into(),
                    after: TimeDelta::hours(5)
                },
                &EntryKind::Closed,
            ]
        );
        assert_eq!(pages[0].groups[0].entries[2].text, "got access");

        // Starred: the blocker and its Unblock message.
        let pages = build(&b, Period::Day, true, &Utc);
        assert_eq!(pages[0].groups[0].entries.len(), 2);
    }

    /// "P" ─┬─ "S" ── "G"
    ///      └─ "T"
    /// P has entries on the 12th only; S on the 13th; G and T on the 14th.
    fn family() -> Board {
        let mut b = Board::default();
        let p = b.add_task("P".into(), "", None, at(12, 9));
        let s = b.add_task("S".into(), "", Some(p), at(13, 9));
        b.add_task("G".into(), "", Some(s), at(14, 10));
        let t = b.add_task("T".into(), "", Some(p), at(14, 9));
        b.task_mut(t)
            .unwrap()
            .post("starred", MessageKind::Block, at(14, 11));
        b
    }

    fn titles(groups: &[TaskGroup]) -> Vec<&str> {
        groups.iter().map(|g| g.title.as_str()).collect()
    }

    #[test]
    fn subtasks_nest_under_their_parent() {
        let pages = build(&family(), Period::Week, false, &Utc);
        let p = &pages[0].groups[0];
        assert_eq!(titles(&pages[0].groups), ["P"]);
        assert_eq!(p.entries.len(), 1);
        assert_eq!(titles(&p.subgroups), ["S", "T"]);
        assert_eq!(titles(&p.subgroups[0].subgroups), ["G"]);
    }

    #[test]
    fn parents_without_entries_still_head_their_subtasks() {
        let pages = build(&family(), Period::Day, false, &Utc);
        let day14 = &pages[0];
        assert_eq!(day14.start, date(14));
        let p = &day14.groups[0];
        assert_eq!(titles(&day14.groups), ["P"]);
        assert!(p.entries.is_empty());
        // T's first entry (09:00) is before G's (10:00), which sits under an empty S.
        assert_eq!(titles(&p.subgroups), ["T", "S"]);
        assert!(p.subgroups[1].entries.is_empty());
        assert_eq!(titles(&p.subgroups[1].subgroups), ["G"]);
    }

    #[test]
    fn starred_only_nests_the_same_way() {
        let pages = build(&family(), Period::Week, true, &Utc);
        assert_eq!(pages.len(), 1);
        let p = &pages[0].groups[0];
        assert!(p.entries.is_empty());
        assert_eq!(titles(&p.subgroups), ["T"]);
        assert_eq!(p.subgroups[0].entries.len(), 1);
    }
}
