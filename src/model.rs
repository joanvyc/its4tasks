use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Deserializer, Serialize};

pub const SLOT_COUNT: usize = 4;

/// How long without updates before a task is shown fully red.
const STALE_AFTER_SECS: f32 = 7.0 * 24.0 * 3600.0;

/// A comment that needs someone else to act before the task can move on.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Blocker {
    Open,
    Resolved { at: DateTime<Utc> },
}

/// What a comment did besides commenting, posted from the comment box.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NoteAction {
    /// Resolved a blocker. Its text and start are copied so this note still
    /// reads right if the blocker is later edited or deleted.
    Unblocked {
        quote: String,
        blocked_since: DateTime<Utc>,
    },
    /// Marked the task done.
    Closed,
}

/// What posting from the comment box does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Comment,
    Block,
    Unblock,
    Close,
}

impl MessageKind {
    pub fn label(self) -> &'static str {
        match self {
            MessageKind::Comment => "Comment",
            MessageKind::Block => "Block",
            MessageKind::Unblock => "Unblock",
            MessageKind::Close => "Close",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub at: DateTime<Utc>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<Blocker>,
    /// When the comment became blocking; `None` in older files, where the
    /// post time stands in for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub starred: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<NoteAction>,
}

impl Note {
    fn new(text: String, at: DateTime<Utc>) -> Self {
        Self {
            at,
            text,
            blocker: None,
            blocked_at: None,
            starred: false,
            action: None,
        }
    }

    pub fn is_open_blocker(&self) -> bool {
        self.blocker == Some(Blocker::Open)
    }

    pub fn blocked_since(&self) -> DateTime<Utc> {
        self.blocked_at.unwrap_or(self.at)
    }

    /// How long an open blocker has been blocking.
    pub fn blocked_for(&self, now: DateTime<Utc>) -> Option<TimeDelta> {
        self.is_open_blocker().then(|| now - self.blocked_since())
    }

    /// How long a resolved blocker was blocking.
    pub fn blocked_duration(&self) -> Option<TimeDelta> {
        match self.blocker {
            Some(Blocker::Resolved { at }) => Some(at - self.blocked_since()),
            _ => None,
        }
    }

    /// How long the blocker this note resolved had been blocking.
    pub fn unblocked_after(&self) -> Option<TimeDelta> {
        match &self.action {
            Some(NoteAction::Unblocked { blocked_since, .. }) => Some(self.at - *blocked_since),
            _ => None,
        }
    }

    /// Blocking comments are starred so they show up in the starred log.
    fn block(&mut self, now: DateTime<Utc>) {
        self.blocker = Some(Blocker::Open);
        self.blocked_at = Some(now);
        self.starred = true;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Active,
    Blocked,
    Done,
}

impl TaskState {
    pub fn label(self) -> &'static str {
        match self {
            TaskState::Active => "Active",
            TaskState::Blocked => "Blocked",
            TaskState::Done => "Done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: u64,
    pub title: String,
    #[serde(default, deserialize_with = "legacy_notes")]
    pub notes: Vec<Note>,
    #[serde(default)]
    pub done: bool,
    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<DateTime<Utc>>,
}

impl Task {
    pub fn touch(&mut self, now: DateTime<Utc>) {
        self.updated_at = now;
    }

    pub fn set_done(&mut self, done: bool, now: DateTime<Utc>) {
        self.done = done;
        self.done_at = done.then_some(now);
        self.touch(now);
    }

    /// Post a comment from the comment box. Blank text is ignored.
    /// - `Block` posts it as an open blocker.
    /// - `Unblock` resolves the oldest open blocker, with this as the message
    ///   (a plain comment if nothing is blocking).
    /// - `Close` marks the task done, with this as the message.
    pub fn post(&mut self, text: &str, kind: MessageKind, now: DateTime<Utc>) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let mut note = Note::new(text.to_owned(), now);
        match kind {
            MessageKind::Comment => {}
            MessageKind::Block => note.block(now),
            MessageKind::Unblock => {
                if let Some(idx) = self.notes.iter().position(Note::is_open_blocker) {
                    let blocker = &self.notes[idx];
                    note.action = Some(NoteAction::Unblocked {
                        quote: blocker.text.clone(),
                        blocked_since: blocker.blocked_since(),
                    });
                    note.starred = true;
                    self.resolve(idx, now);
                }
            }
            MessageKind::Close => {
                note.action = Some(NoteAction::Closed);
                self.set_done(true, now);
            }
        }
        self.notes.push(note);
        self.touch(now);
    }

    /// Replace a comment's text. Blank text is ignored.
    pub fn edit_note(&mut self, idx: usize, text: &str, now: DateTime<Utc>) {
        let text = text.trim();
        if let Some(note) = self.notes.get_mut(idx)
            && !text.is_empty()
            && note.text != text
        {
            note.text = text.to_owned();
            self.touch(now);
        }
    }

    /// What the comment box offers for this task, the default first.
    pub fn message_kinds(&self) -> Vec<MessageKind> {
        let mut kinds = vec![MessageKind::Comment];
        kinds.push(if self.open_blocker().is_some() {
            MessageKind::Unblock
        } else {
            MessageKind::Block
        });
        if !self.done {
            kinds.push(MessageKind::Close);
        }
        kinds
    }

    pub fn set_blocking(&mut self, idx: usize, now: DateTime<Utc>) {
        if let Some(note) = self.notes.get_mut(idx) {
            note.block(now);
            self.touch(now);
        }
    }

    pub fn resolve(&mut self, idx: usize, now: DateTime<Utc>) {
        if let Some(note) = self.notes.get_mut(idx)
            && note.is_open_blocker()
        {
            note.blocker = Some(Blocker::Resolved { at: now });
            self.touch(now);
        }
    }

    /// Undo a blocking mark (e.g. a mis-click); doesn't count as an update.
    pub fn unmark_blocking(&mut self, idx: usize) {
        if let Some(note) = self.notes.get_mut(idx) {
            note.blocker = None;
            note.blocked_at = None;
        }
    }

    /// Doesn't count as an update.
    pub fn toggle_star(&mut self, idx: usize) {
        if let Some(note) = self.notes.get_mut(idx) {
            note.starred = !note.starred;
        }
    }

    pub fn open_blocker(&self) -> Option<&Note> {
        self.notes.iter().find(|n| n.is_open_blocker())
    }

    /// Derived from `done` and the comments, never stored.
    pub fn state(&self) -> TaskState {
        if self.done {
            TaskState::Done
        } else if self.open_blocker().is_some() {
            TaskState::Blocked
        } else {
            TaskState::Active
        }
    }

    pub fn delete_note(&mut self, idx: usize) {
        if idx < self.notes.len() {
            self.notes.remove(idx);
        }
    }
}

/// Accept both the current comment list and the old single `notes` string,
/// which becomes one comment stamped with the load time.
fn legacy_notes<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Note>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Notes {
        Legacy(String),
        Log(Vec<Note>),
    }
    Ok(match Notes::deserialize(d)? {
        Notes::Legacy(text) if text.trim().is_empty() => Vec::new(),
        Notes::Legacy(text) => vec![Note::new(text.trim().to_owned(), Utc::now())],
        Notes::Log(notes) => notes,
    })
}

/// A multi-line comment's first line is its title: `(Some(title), rest)`.
/// A single-line comment has no title: `(None, text)`.
pub fn split_title(text: &str) -> (Option<&str>, &str) {
    match text.split_once('\n') {
        Some((title, body)) if !body.trim().is_empty() => (Some(title.trim_end()), body),
        _ => (None, text.trim_end()),
    }
}

/// How a comment is referred to elsewhere: its title, or all of it if it's one line.
pub fn headline(text: &str) -> &str {
    match split_title(text) {
        (Some(title), _) => title,
        (None, text) => text,
    }
}

/// Compact duration: `45m`, `5h`, `1d 4h`, `12d`.
pub fn short_duration(d: TimeDelta) -> String {
    let (days, hours, minutes) = (d.num_days(), d.num_hours() % 24, d.num_minutes());
    match days {
        0 if d.num_hours() == 0 => format!("{}m", minutes.max(0)),
        0 => format!("{hours}h"),
        1..7 if hours > 0 => format!("{days}d {hours}h"),
        _ => format!("{days}d"),
    }
}

/// 0.0 for a task updated just now, rising to 1.0 once it is `STALE_AFTER_SECS` old.
pub fn staleness(age: TimeDelta) -> f32 {
    (age.num_seconds() as f32 / STALE_AFTER_SECS).clamp(0.0, 1.0)
}

/// All tasks plus the ones currently brought into focus.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Board {
    pub tasks: Vec<Task>,
    pub slots: [Option<u64>; SLOT_COUNT],
    pub next_id: u64,
}

impl Board {
    pub fn add_task(&mut self, title: String, first_note: &str, now: DateTime<Utc>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut task = Task {
            id,
            title,
            notes: Vec::new(),
            done: false,
            updated_at: now,
            created_at: Some(now),
            done_at: None,
        };
        task.post(first_note, MessageKind::Comment, now);
        self.tasks.push(task);
        id
    }

    pub fn delete_task(&mut self, id: u64) {
        self.tasks.retain(|t| t.id != id);
        for slot in &mut self.slots {
            if *slot == Some(id) {
                *slot = None;
            }
        }
    }

    pub fn task(&self, id: u64) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn task_mut(&mut self, id: u64) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    pub fn slot_of(&self, id: u64) -> Option<usize> {
        self.slots.iter().position(|s| *s == Some(id))
    }

    pub fn has_free_slot(&self) -> bool {
        self.slots.iter().any(Option::is_none)
    }

    /// Put the task into the first empty slot. Returns false if it is already
    /// focused, doesn't exist, or every slot is taken.
    pub fn focus(&mut self, id: u64) -> bool {
        if self.slot_of(id).is_some() || self.task(id).is_none() {
            return false;
        }
        match self.slots.iter_mut().find(|s| s.is_none()) {
            Some(slot) => {
                *slot = Some(id);
                true
            }
            None => false,
        }
    }

    /// Put the task into a specific slot, replacing whatever was there and
    /// moving it out of any other slot it occupied.
    pub fn focus_into(&mut self, slot: usize, id: u64) {
        if slot >= SLOT_COUNT || self.task(id).is_none() {
            return;
        }
        if let Some(old) = self.slot_of(id) {
            self.slots[old] = None;
        }
        self.slots[slot] = Some(id);
    }

    pub fn unfocus(&mut self, slot: usize) {
        if let Some(s) = self.slots.get_mut(slot) {
            *s = None;
        }
    }

    /// Drop slot references to tasks that no longer exist (e.g. a hand-edited file).
    pub fn sanitize(&mut self) {
        for i in 0..SLOT_COUNT {
            if let Some(id) = self.slots[i]
                && self.task(id).is_none()
            {
                self.slots[i] = None;
            }
        }
        let max_id = self.tasks.iter().map(|t| t.id + 1).max().unwrap_or(0);
        self.next_id = self.next_id.max(max_id);
    }
}

/// Case-insensitive match over title and notes. An empty filter matches everything.
pub fn matches_filter(task: &Task, filter: &str) -> bool {
    let filter = filter.trim().to_lowercase();
    filter.is_empty()
        || task.title.to_lowercase().contains(&filter)
        || task
            .notes
            .iter()
            .any(|n| n.text.to_lowercase().contains(&filter))
}

/// Whether the task list shows this task: it matches the filter, and isn't
/// closed while closed tasks are hidden.
pub fn shown_in_list(task: &Task, filter: &str, hide_closed: bool) -> bool {
    matches_filter(task, filter) && !(hide_closed && task.done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board_with(n: usize) -> Board {
        let mut b = Board::default();
        for i in 0..n {
            b.add_task(format!("task {i}"), "", Utc::now());
        }
        b
    }

    #[test]
    fn focus_fills_first_free_slot_until_full() {
        let mut b = board_with(5);
        for id in 0..4 {
            assert!(b.focus(id));
        }
        assert_eq!(b.slots, [Some(0), Some(1), Some(2), Some(3)]);
        assert!(!b.has_free_slot());
        assert!(!b.focus(4));
    }

    #[test]
    fn focus_twice_is_noop() {
        let mut b = board_with(2);
        assert!(b.focus(0));
        assert!(!b.focus(0));
        assert_eq!(b.slots, [Some(0), None, None, None]);
    }

    #[test]
    fn unfocus_frees_slot_and_keeps_task() {
        let mut b = board_with(2);
        b.focus(0);
        b.focus(1);
        b.unfocus(0);
        assert_eq!(b.slots, [None, Some(1), None, None]);
        assert_eq!(b.tasks.len(), 2);
        assert!(b.focus(0));
        assert_eq!(b.slot_of(0), Some(0));
    }

    #[test]
    fn focus_into_replaces_and_moves() {
        let mut b = board_with(3);
        b.focus(0);
        b.focus_into(0, 1);
        assert_eq!(b.slots, [Some(1), None, None, None]);
        b.focus_into(2, 1);
        assert_eq!(b.slots, [None, None, Some(1), None]);
    }

    #[test]
    fn delete_clears_slot() {
        let mut b = board_with(2);
        b.focus(1);
        b.delete_task(1);
        assert_eq!(b.slots, [None; SLOT_COUNT]);
        assert!(b.task(1).is_none());
    }

    #[test]
    fn filter_matches_title_and_notes_case_insensitively() {
        let mut t = Task {
            id: 0,
            title: "Write Report".into(),
            notes: Vec::new(),
            done: false,
            updated_at: Utc::now(),
            created_at: None,
            done_at: None,
        };
        t.post("send to Alice", MessageKind::Comment, Utc::now());
        assert!(matches_filter(&t, ""));
        assert!(matches_filter(&t, "report"));
        assert!(matches_filter(&t, "  ALICE "));
        assert!(!matches_filter(&t, "bob"));
    }

    #[test]
    fn hide_closed_drops_done_tasks_and_keeps_the_filter() {
        let mut b = board_with(2);
        b.task_mut(1).unwrap().set_done(true, Utc::now());
        let shown = |filter, hide| {
            b.tasks
                .iter()
                .filter(|t| shown_in_list(t, filter, hide))
                .map(|t| t.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(shown("", false), [0, 1]);
        assert_eq!(shown("", true), [0]);
        assert_eq!(shown("task 1", false), [1]);
        assert_eq!(shown("task 1", true), Vec::<u64>::new());
    }

    #[test]
    fn serde_round_trip() {
        let mut b = board_with(3);
        b.focus(2);
        b.task_mut(0).unwrap().done = true;
        let json = serde_json::to_string(&b).unwrap();
        let back: Board = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn legacy_string_notes_become_one_comment() {
        let json = r#"{"id":0,"title":"t","notes":"  old notes "}"#;
        let t: Task = serde_json::from_str(json).unwrap();
        assert_eq!(t.notes.len(), 1);
        assert_eq!(t.notes[0].text, "old notes");

        let json = r#"{"id":0,"title":"t","notes":""}"#;
        let t: Task = serde_json::from_str(json).unwrap();
        assert!(t.notes.is_empty());
    }

    #[test]
    fn post_touches_and_skips_blank() {
        let mut b = board_with(1);
        let t = b.task_mut(0).unwrap();
        let later = t.updated_at + TimeDelta::hours(3);
        t.post("   ", MessageKind::Comment, later);
        assert!(t.notes.is_empty());
        assert!(t.updated_at < later);
        t.post("hi", MessageKind::Comment, later);
        assert_eq!(t.notes.len(), 1);
        assert_eq!(t.updated_at, later);
        t.delete_note(5);
        t.delete_note(0);
        assert!(t.notes.is_empty());
    }

    #[test]
    fn blockers_drive_state() {
        let mut b = board_with(1);
        let now = Utc::now();
        let t = b.task_mut(0).unwrap();
        assert_eq!(t.state(), TaskState::Active);
        t.post("waiting on new input case", MessageKind::Block, now);
        t.post("plain comment", MessageKind::Comment, now);
        t.post("also need access", MessageKind::Comment, now);
        t.set_blocking(2, now);
        assert_eq!(t.state(), TaskState::Blocked);

        t.resolve(0, now);
        assert_eq!(t.state(), TaskState::Blocked);
        assert_eq!(t.notes[0].blocker, Some(Blocker::Resolved { at: now }));
        t.resolve(1, now); // not a blocker: stays untouched
        assert_eq!(t.notes[1].blocker, None);
        t.resolve(2, now);
        assert_eq!(t.state(), TaskState::Active);

        t.set_blocking(1, now);
        t.done = true;
        assert_eq!(t.state(), TaskState::Done);
        t.done = false;
        t.unmark_blocking(1);
        assert_eq!(t.state(), TaskState::Active);
    }

    #[test]
    fn blockers_round_trip_and_old_notes_load() {
        let mut b = board_with(1);
        let t = b.task_mut(0).unwrap();
        t.post("open", MessageKind::Block, Utc::now());
        t.post("resolved", MessageKind::Block, Utc::now());
        t.resolve(1, Utc::now());
        let json = serde_json::to_string(&b).unwrap();
        let back: Board = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);

        let json = r#"{"id":0,"title":"t","notes":[{"at":"2026-10-01T10:00:00Z","text":"x"}]}"#;
        let t: Task = serde_json::from_str(json).unwrap();
        assert_eq!(t.notes[0].blocker, None);
        assert_eq!(t.state(), TaskState::Active);
    }

    #[test]
    fn blocking_stars_and_tracks_durations() {
        let mut b = board_with(1);
        let t0 = Utc::now();
        let t = b.task_mut(0).unwrap();
        t.post("posted as blocker", MessageKind::Block, t0);
        t.post("plain", MessageKind::Comment, t0);
        assert!(t.notes[0].starred);
        assert!(!t.notes[1].starred);
        assert_eq!(
            t.notes[0].blocked_for(t0 + TimeDelta::hours(5)),
            Some(TimeDelta::hours(5))
        );

        let later = t0 + TimeDelta::hours(2);
        t.set_blocking(1, later);
        assert!(t.notes[1].starred);
        assert_eq!(t.notes[1].blocked_since(), later);
        t.resolve(1, later + TimeDelta::hours(3));
        assert_eq!(t.notes[1].blocked_duration(), Some(TimeDelta::hours(3)));
        assert_eq!(t.notes[1].blocked_for(later), None);

        t.toggle_star(0);
        assert!(!t.notes[0].starred);
        t.unmark_blocking(1);
        assert_eq!(t.notes[1].blocked_at, None);
    }

    #[test]
    fn unblock_resolves_oldest_blocker_with_a_message() {
        let mut b = board_with(1);
        let t0 = Utc::now();
        let t = b.task_mut(0).unwrap();
        assert_eq!(
            t.message_kinds(),
            [MessageKind::Comment, MessageKind::Block, MessageKind::Close]
        );
        t.post("need access", MessageKind::Block, t0);
        t.post("need data", MessageKind::Block, t0 + TimeDelta::hours(1));
        assert_eq!(
            t.message_kinds(),
            [
                MessageKind::Comment,
                MessageKind::Unblock,
                MessageKind::Close
            ]
        );

        let later = t0 + TimeDelta::hours(5);
        t.post("got access", MessageKind::Unblock, later);
        assert_eq!(t.notes[0].blocker, Some(Blocker::Resolved { at: later }));
        assert!(t.notes[1].is_open_blocker());
        let msg = &t.notes[2];
        assert_eq!(
            msg.action,
            Some(NoteAction::Unblocked {
                quote: "need access".into(),
                blocked_since: t0
            })
        );
        assert_eq!(msg.unblocked_after(), Some(TimeDelta::hours(5)));
        assert!(msg.starred);

        t.post("got data", MessageKind::Unblock, later);
        assert_eq!(t.state(), TaskState::Active);
        // Nothing left to resolve: a plain comment.
        t.post("extra", MessageKind::Unblock, later);
        assert_eq!(t.notes[4].action, None);
    }

    #[test]
    fn first_line_is_a_title_only_when_more_lines_follow() {
        assert_eq!(split_title("one line"), (None, "one line"));
        assert_eq!(split_title("one line\n  \n"), (None, "one line"));
        assert_eq!(
            split_title("Title \nbody\nmore"),
            (Some("Title"), "body\nmore")
        );
        assert_eq!(headline("Title\nbody"), "Title");
        assert_eq!(headline("just this"), "just this");
    }

    #[test]
    fn edit_note_replaces_text_and_touches() {
        let mut b = board_with(1);
        let t0 = Utc::now();
        let t = b.task_mut(0).unwrap();
        t.post("frist", MessageKind::Comment, t0);
        let later = t0 + TimeDelta::hours(1);
        t.edit_note(0, "   ", later);
        t.edit_note(3, "nope", later);
        assert_eq!(t.notes[0].text, "frist");
        assert_eq!(t.updated_at, t0);
        t.edit_note(0, " first\nwith details ", later);
        assert_eq!(t.notes[0].text, "first\nwith details");
        assert_eq!(t.notes[0].at, t0);
        assert_eq!(t.updated_at, later);
    }

    #[test]
    fn close_marks_done_with_a_message() {
        let mut b = board_with(1);
        let now = Utc::now();
        let t = b.task_mut(0).unwrap();
        t.post("   ", MessageKind::Close, now);
        assert!(!t.done);
        t.post("shipped", MessageKind::Close, now);
        assert_eq!(t.state(), TaskState::Done);
        assert_eq!(t.done_at, Some(now));
        assert_eq!(t.notes[0].action, Some(NoteAction::Closed));
        assert_eq!(
            t.message_kinds(),
            [MessageKind::Comment, MessageKind::Block]
        );

        let json = serde_json::to_string(&b).unwrap();
        let back: Board = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn set_done_records_time() {
        let mut b = board_with(1);
        let now = Utc::now();
        let t = b.task_mut(0).unwrap();
        t.set_done(true, now);
        assert_eq!(t.done_at, Some(now));
        t.set_done(false, now);
        assert_eq!(t.done_at, None);
    }

    #[test]
    fn short_duration_formats() {
        assert_eq!(short_duration(TimeDelta::minutes(45)), "45m");
        assert_eq!(short_duration(TimeDelta::hours(5)), "5h");
        assert_eq!(short_duration(TimeDelta::hours(28)), "1d 4h");
        assert_eq!(short_duration(TimeDelta::days(3)), "3d");
        assert_eq!(
            short_duration(TimeDelta::days(12) + TimeDelta::hours(5)),
            "12d"
        );
        assert_eq!(short_duration(TimeDelta::seconds(-5)), "0m");
    }

    #[test]
    fn staleness_ramps_and_clamps() {
        assert_eq!(staleness(TimeDelta::zero()), 0.0);
        assert_eq!(staleness(TimeDelta::days(-1)), 0.0);
        assert!((staleness(TimeDelta::hours(84)) - 0.5).abs() < 1e-4);
        assert_eq!(staleness(TimeDelta::days(7)), 1.0);
        assert_eq!(staleness(TimeDelta::days(30)), 1.0);
    }

    #[test]
    fn sanitize_drops_dangling_slots_and_fixes_next_id() {
        let mut b = board_with(2);
        b.slots[3] = Some(99);
        b.next_id = 0;
        b.sanitize();
        assert_eq!(b.slots[3], None);
        assert_eq!(b.next_id, 2);
    }
}
