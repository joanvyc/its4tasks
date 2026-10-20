use std::{collections::HashSet, fmt, iter};

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

pub const SLOT_COUNT: usize = 4;

/// How long without updates before a task is shown fully red.
const STALE_AFTER_SECS: f32 = 7.0 * 24.0 * 3600.0;

/// A comment that needs someone else to act before the task can move on.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Blocker {
    Open,
    Resolved { at: DateTime<Utc> },
}

/// A comment saying what to do next, which is up to me rather than someone
/// else (unlike a `Blocker`). `since` is when it became a step.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum NextStep {
    Open {
        since: DateTime<Utc>,
    },
    Done {
        since: DateTime<Utc>,
        at: DateTime<Utc>,
    },
    /// Grew into its own subtask.
    Promoted {
        since: DateTime<Utc>,
        at: DateTime<Utc>,
        subtask: Uuid,
    },
}

impl NextStep {
    pub fn since(self) -> DateTime<Utc> {
        match self {
            NextStep::Open { since }
            | NextStep::Done { since, .. }
            | NextStep::Promoted { since, .. } => since,
        }
    }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageKind {
    Comment,
    Star,
    Next,
    Block,
    Unblock,
    Close,
}

impl MessageKind {
    pub fn label(self) -> &'static str {
        match self {
            MessageKind::Comment => "Comment",
            MessageKind::Star => "Star",
            MessageKind::Next => "Next step",
            MessageKind::Block => "Block",
            MessageKind::Unblock => "Unblock",
            MessageKind::Close => "Close",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    /// Stays the same when the comment is edited or moved to another task.
    /// Files from before ids get fresh ones.
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
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
    /// A comment is a blocker or a next step, never both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_step: Option<NextStep>,
}

impl Note {
    fn new(text: String, at: DateTime<Utc>) -> Self {
        Self {
            id: Uuid::new_v4(),
            at,
            text,
            blocker: None,
            blocked_at: None,
            starred: false,
            action: None,
            next_step: None,
        }
    }

    pub fn is_open_blocker(&self) -> bool {
        self.blocker == Some(Blocker::Open)
    }

    pub fn is_open_step(&self) -> bool {
        matches!(self.next_step, Some(NextStep::Open { .. }))
    }

    /// A plain comment, which can become a blocker or a next step.
    fn is_plain(&self) -> bool {
        self.blocker.is_none() && self.next_step.is_none() && self.action.is_none()
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

    /// Next steps are starred, like blockers, so they show up in the starred log.
    fn make_step(&mut self, now: DateTime<Utc>) {
        self.next_step = Some(NextStep::Open { since: now });
        self.starred = true;
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
    /// Has an open next step.
    Ready,
    Blocked,
    Done,
}

impl TaskState {
    pub fn label(self) -> &'static str {
        match self {
            TaskState::Active => "Active",
            TaskState::Ready => "Next step",
            TaskState::Blocked => "Blocked",
            TaskState::Done => "Done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
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
    /// The task this is a subtask of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Uuid>,
    /// Its subtasks are hidden in the task list (unless on the board).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub folded: bool,
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
    /// - `Star` posts it starred.
    /// - `Next` posts it as an open next step (starred).
    /// - `Block` posts it as an open blocker (a plain comment if something is
    ///   already blocking; see `can_block`).
    /// - `Unblock` resolves the open blocker, with this as the message
    ///   (a plain comment if nothing is blocking).
    /// - `Close` marks the task done, with this as the message.
    pub fn post(&mut self, text: &str, kind: MessageKind, now: DateTime<Utc>) {
        self.post_as(Uuid::new_v4(), text, kind, now);
    }

    /// `post`, giving the comment this id.
    pub fn post_as(&mut self, id: Uuid, text: &str, kind: MessageKind, now: DateTime<Utc>) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let mut note = Note::new(text.to_owned(), now);
        note.id = id;
        match kind {
            MessageKind::Comment => {}
            MessageKind::Star => note.starred = true,
            MessageKind::Next => note.make_step(now),
            MessageKind::Block if self.can_block() => note.block(now),
            MessageKind::Block => {}
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
        let mut kinds = vec![MessageKind::Comment, MessageKind::Star, MessageKind::Next];
        kinds.push(if self.can_block() {
            MessageKind::Block
        } else {
            MessageKind::Unblock
        });
        if !self.done {
            kinds.push(MessageKind::Close);
        }
        kinds
    }

    /// Only one comment blocks at a time, so it's always clear what an
    /// Unblock resolves.
    pub fn can_block(&self) -> bool {
        self.open_blocker().is_none()
    }

    /// Turn a comment into the blocker. Does nothing while another one
    /// blocks, or on a next step.
    pub fn set_blocking(&mut self, idx: usize, now: DateTime<Utc>) {
        if !self.can_block() {
            return;
        }
        if let Some(note) = self.notes.get_mut(idx)
            && note.next_step.is_none()
        {
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

    /// Turn a plain comment into an open next step (starred).
    pub fn mark_next_step(&mut self, idx: usize, now: DateTime<Utc>) {
        if let Some(note) = self.notes.get_mut(idx)
            && note.is_plain()
        {
            note.make_step(now);
            self.touch(now);
        }
    }

    /// Tick an open next step off, or reopen a done one.
    pub fn toggle_step(&mut self, idx: usize, now: DateTime<Utc>) {
        let Some(note) = self.notes.get_mut(idx) else {
            return;
        };
        note.next_step = match note.next_step {
            Some(NextStep::Open { since }) => Some(NextStep::Done { since, at: now }),
            Some(NextStep::Done { since, .. }) => Some(NextStep::Open { since }),
            _ => return,
        };
        self.touch(now);
    }

    /// Undo a next step mark (e.g. a mis-click); doesn't count as an update.
    /// A promoted step stays, since its subtask came from it.
    pub fn unmark_next_step(&mut self, idx: usize) {
        if let Some(note) = self.notes.get_mut(idx)
            && !matches!(note.next_step, Some(NextStep::Promoted { .. }))
        {
            note.next_step = None;
        }
    }

    pub fn open_steps(&self) -> impl Iterator<Item = &Note> {
        self.notes.iter().filter(|n| n.is_open_step())
    }

    /// Doesn't count as an update.
    pub fn toggle_fold(&mut self) {
        self.folded = !self.folded;
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
        } else if self.open_steps().next().is_some() {
            TaskState::Ready
        } else {
            TaskState::Active
        }
    }

    /// Read the comment box's text. Text starting with `/` is a command, with
    /// anything after it as its message (or subtask title); `//` escapes the
    /// slash. Commands that can't apply to this task are errors.
    pub fn parse_input<'a>(&self, text: &'a str) -> Parsed<'a> {
        let plain = |text| Parsed {
            command: None,
            token_len: None,
            result: Ok(Submit::Post(None, text)),
            partial: false,
            separated: false,
        };
        let Some(rest) = text.strip_prefix('/') else {
            return plain(text);
        };
        if rest.starts_with('/') {
            return plain(rest);
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (name, arg) = rest.split_at(end);
        let command = COMMANDS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c);
        let partial = command.is_none() && COMMANDS.iter().any(|(n, _)| n.starts_with(name));
        let message = arg.trim();
        let result = match command {
            None => Err(CommandError::Unknown(format!("/{name}"))),
            Some(Command::Unblock) if self.open_blocker().is_none() => {
                Err(CommandError::NothingToUnblock)
            }
            Some(Command::Block) if !self.can_block() => Err(CommandError::AlreadyBlocked),
            Some(Command::Close) if self.done => Err(CommandError::AlreadyClosed),
            Some(command) => Ok(match (command.kind(), message) {
                (None, title) => Submit::NewSubtask(title),
                (Some(kind), "") => Submit::SetKind(kind),
                (Some(kind), message) => Submit::Post(Some(kind), message),
            }),
        };
        Parsed {
            command,
            token_len: Some(1 + end),
            result,
            partial,
            separated: !arg.is_empty(),
        }
    }

    pub fn delete_note(&mut self, idx: usize) {
        if idx < self.notes.len() {
            self.notes.remove(idx);
        }
    }

    /// The comment and the one tied to it, which always move together: a
    /// resolved blocker and the Unblock message that resolved it. Paired by
    /// time, since the message only keeps a copy of the blocker. In order.
    pub fn linked_notes(&self, idx: usize) -> Vec<usize> {
        let Some(note) = self.notes.get(idx) else {
            return Vec::new();
        };
        let resolves = |blocker: &Note, message: &Note| match (blocker.blocker, &message.action) {
            (Some(Blocker::Resolved { at }), Some(NoteAction::Unblocked { blocked_since, .. })) => {
                message.at == at && *blocked_since == blocker.blocked_since()
            }
            _ => false,
        };
        let partner =
            self.notes.iter().enumerate().position(|(i, other)| {
                i != idx && (resolves(note, other) || resolves(other, note))
            });
        let mut linked: Vec<usize> = iter::once(idx).chain(partner).collect();
        linked.sort_unstable();
        linked
    }

    pub fn note_index(&self, id: Uuid) -> Option<usize> {
        self.notes.iter().position(|n| n.id == id)
    }

    /// Insert a comment where its time puts it.
    fn insert_note(&mut self, note: Note) {
        let at = self.notes.partition_point(|n| n.at <= note.at);
        self.notes.insert(at, note);
    }
}

/// Why a comment can't move to a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MoveError {
    NotFound,
    SameTask,
    TargetClosed,
    CloseNote,
    Promoted,
    AlreadyBlocked,
    BlockedBeforeResolved,
}

impl fmt::Display for MoveError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            MoveError::NotFound => "The comment or task no longer exists",
            MoveError::SameTask => "The comment is already on this task",
            MoveError::TargetClosed => "The task is closed",
            MoveError::CloseNote => "A closing comment can't be moved: it closed its task",
            MoveError::Promoted => "A promoted step can't be moved: its subtask is on this task",
            MoveError::AlreadyBlocked => "Already blocked: it has an open blocker",
            MoveError::BlockedBeforeResolved => {
                "Its open blocker started before this blocker was resolved"
            }
        })
    }
}

/// A `/command` typed at the start of the comment box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Block,
    Unblock,
    Close,
    Star,
    Next,
    Subtask,
}

/// Names a command can be typed as.
const COMMANDS: [(&str, Command); 8] = [
    ("next", Command::Next),
    ("block", Command::Block),
    ("unblock", Command::Unblock),
    ("release", Command::Unblock),
    ("close", Command::Close),
    ("star", Command::Star),
    ("sub-task", Command::Subtask),
    ("subtask", Command::Subtask),
];

impl Command {
    /// The kind of message it posts; `None` for `/sub-task`, which posts nothing.
    pub fn kind(self) -> Option<MessageKind> {
        match self {
            Command::Block => Some(MessageKind::Block),
            Command::Unblock => Some(MessageKind::Unblock),
            Command::Close => Some(MessageKind::Close),
            Command::Star => Some(MessageKind::Star),
            Command::Next => Some(MessageKind::Next),
            Command::Subtask => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    /// The typed token, slash included.
    Unknown(String),
    NothingToUnblock,
    AlreadyBlocked,
    AlreadyClosed,
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CommandError::Unknown(name) => write!(
                f,
                "Unknown command {name}. Try /next, /block, /unblock, /close, /star or /sub-task"
            ),
            CommandError::NothingToUnblock => write!(f, "Nothing to unblock: no open blocker"),
            CommandError::AlreadyBlocked => {
                write!(
                    f,
                    "Already blocked: unblock it before adding another blocker"
                )
            }
            CommandError::AlreadyClosed => write!(f, "The task is already closed"),
        }
    }
}

/// What pressing Enter in the comment box does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submit<'a> {
    /// Post this text, as the given kind or (`None`) the picker's.
    Post(Option<MessageKind>, &'a str),
    /// A command without a message: switch the picker to it.
    SetKind(MessageKind),
    /// Open the new subtask dialog with this title (maybe empty).
    NewSubtask(&'a str),
}

/// The comment box's text, read as a plain message or a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed<'a> {
    pub command: Option<Command>,
    /// Byte length of the `/command` token; `None` for a plain message.
    pub token_len: Option<usize>,
    pub result: Result<Submit<'a>, CommandError>,
    /// The token is still the start of a known command, e.g. `/blo`.
    pub partial: bool,
    /// Whitespace follows the token, so the command is fully typed.
    separated: bool,
}

impl Parsed<'_> {
    /// The fully typed, valid command, for showing what Enter will do.
    pub fn preview(&self) -> Option<Command> {
        self.command
            .filter(|_| self.separated && self.result.is_ok())
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
    pub slots: [Option<Uuid>; SLOT_COUNT],
}

impl Board {
    pub fn add_task(
        &mut self,
        title: String,
        first_note: &str,
        parent: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> Uuid {
        // Time-ordered, so sorting by id keeps tasks in creation order.
        let id = Uuid::now_v7();
        self.add_task_as(id, Uuid::new_v4(), title, first_note, parent, now);
        id
    }

    /// `add_task`, giving the task `id` and its first comment `note_id`.
    pub fn add_task_as(
        &mut self,
        id: Uuid,
        note_id: Uuid,
        title: String,
        first_note: &str,
        parent: Option<Uuid>,
        now: DateTime<Utc>,
    ) {
        let mut task = Task {
            id,
            title,
            notes: Vec::new(),
            done: false,
            updated_at: now,
            created_at: Some(now),
            done_at: None,
            parent,
            folded: false,
        };
        task.post_as(note_id, first_note, MessageKind::Comment, now);
        self.tasks.push(task);
    }

    /// Turn an open next step into a subtask with this title and first
    /// comment. The subtask keeps the step's history: created when the step
    /// was posted, last updated when it became a step, its first comment as
    /// old (and as starred) as the step. The step stays behind as a marker
    /// pointing at it. `None` if the comment isn't an open step any more.
    pub fn promote_step(
        &mut self,
        task_id: Uuid,
        idx: usize,
        title: String,
        first_note: &str,
        now: DateTime<Utc>,
    ) -> Option<Uuid> {
        let id = Uuid::now_v7();
        self.promote_step_as(id, Uuid::new_v4(), task_id, idx, title, first_note, now)
            .then_some(id)
    }

    /// `promote_step`, giving the subtask `id` and its first comment
    /// `note_id`. False if the comment isn't an open step any more.
    #[allow(clippy::too_many_arguments)]
    pub fn promote_step_as(
        &mut self,
        id: Uuid,
        note_id: Uuid,
        task_id: Uuid,
        idx: usize,
        title: String,
        first_note: &str,
        now: DateTime<Utc>,
    ) -> bool {
        let Some(note) = self.task(task_id).and_then(|t| t.notes.get(idx)) else {
            return false;
        };
        let Some(NextStep::Open { since }) = note.next_step else {
            return false;
        };
        let (posted, starred) = (note.at, note.starred);
        self.add_task_as(id, note_id, title, first_note, Some(task_id), posted);
        if let Some(subtask) = self.task_mut(id) {
            subtask.updated_at = since;
            if let Some(first) = subtask.notes.first_mut() {
                first.starred = starred;
            }
        }
        let Some(task) = self.task_mut(task_id) else {
            return false;
        };
        task.notes[idx].next_step = Some(NextStep::Promoted {
            since,
            at: now,
            subtask: id,
        });
        task.touch(now);
        true
    }

    /// Delete the task and all its subtasks.
    pub fn delete_task(&mut self, id: Uuid) {
        let mut gone = self.descendants(id);
        gone.push(id);
        self.tasks.retain(|t| !gone.contains(&t.id));
        for slot in &mut self.slots {
            if slot.is_some_and(|s| gone.contains(&s)) {
                *slot = None;
            }
        }
    }

    /// Direct subtasks, in list order.
    pub fn children(&self, id: Uuid) -> impl Iterator<Item = &Task> {
        self.tasks.iter().filter(move |t| t.parent == Some(id))
    }

    /// Subtasks at any depth.
    pub fn descendants(&self, id: Uuid) -> Vec<Uuid> {
        let mut found: Vec<Uuid> = Vec::new();
        let mut next = vec![id];
        while let Some(id) = next.pop() {
            for child in self.children(id) {
                if !found.contains(&child.id) {
                    found.push(child.id);
                    next.push(child.id);
                }
            }
        }
        found
    }

    /// Whether `id` sits somewhere below `of`.
    pub fn is_descendant(&self, id: Uuid, of: Uuid) -> bool {
        let mut seen = Vec::new();
        let mut current = self.task(id).and_then(|t| t.parent);
        while let Some(p) = current {
            if p == of {
                return true;
            }
            if seen.contains(&p) {
                return false;
            }
            seen.push(p);
            current = self.task(p).and_then(|t| t.parent);
        }
        false
    }

    /// Whether `set_parent(id, parent)` would succeed.
    pub fn can_set_parent(&self, id: Uuid, parent: Option<Uuid>) -> bool {
        let Some(task) = self.task(id) else {
            return false;
        };
        task.parent != parent
            && match parent {
                None => true,
                Some(p) => p != id && self.task(p).is_some() && !self.is_descendant(p, id),
            }
    }

    /// Make the task a subtask of `parent`, or a root task with `None`.
    /// Refuses unknown tasks and anything that would make a cycle.
    pub fn set_parent(&mut self, id: Uuid, parent: Option<Uuid>) -> bool {
        if !self.can_set_parent(id, parent) {
            return false;
        }
        if let Some(task) = self.task_mut(id) {
            task.parent = parent;
        }
        true
    }

    /// Subtasks that aren't done.
    fn open_children(&self, id: Uuid) -> impl Iterator<Item = &Task> {
        self.children(id).filter(|t| !t.done)
    }

    /// At least one subtask is open, and every open one is blocked.
    pub fn blocked_by_subtasks(&self, id: Uuid) -> bool {
        let mut open = self.open_children(id).peekable();
        open.peek().is_some() && open.all(|t| self.state(t.id) == TaskState::Blocked)
    }

    /// Like `Task::state`, but a task whose open subtasks are all blocked is
    /// blocked too.
    pub fn state(&self, id: Uuid) -> TaskState {
        match self.task(id).map(Task::state) {
            Some(TaskState::Active | TaskState::Ready) if self.blocked_by_subtasks(id) => {
                TaskState::Blocked
            }
            Some(state) => state,
            None => TaskState::Active,
        }
    }

    /// When the task became blocked, from its own oldest open blocker or from
    /// its subtasks (when the last open one got blocked, or the last unblocked
    /// one was closed), whichever came first. `None` unless blocked.
    pub fn blocked_since(&self, id: Uuid) -> Option<DateTime<Utc>> {
        let task = self.task(id)?;
        if task.done {
            return None;
        }
        let own = task
            .notes
            .iter()
            .filter(|n| n.is_open_blocker())
            .map(Note::blocked_since)
            .min();
        let subtasks = if self.blocked_by_subtasks(id) {
            let open = self
                .open_children(id)
                .filter_map(|t| self.blocked_since(t.id));
            let closed = self.children(id).filter_map(|t| t.done_at);
            open.chain(closed).max()
        } else {
            None
        };
        own.into_iter().chain(subtasks).min()
    }

    pub fn blocked_for(&self, id: Uuid, now: DateTime<Utc>) -> Option<TimeDelta> {
        self.blocked_since(id).map(|since| now - since)
    }

    /// The task list: tasks in tree order with their depth. A task shows if it
    /// passes `shown_in_list`, or if one of its subtasks shows. Below a folded
    /// task only subtasks on the board pass; folds don't apply while filtering.
    pub fn list_rows(&self, filter: &str, hide_closed: bool) -> Vec<(Uuid, usize)> {
        fn visit(
            board: &Board,
            task: &Task,
            depth: usize,
            filter: &str,
            hide_closed: bool,
            folded_above: bool,
            rows: &mut Vec<(Uuid, usize)>,
        ) {
            let at = rows.len();
            let folded = folded_above || (task.folded && filter.trim().is_empty());
            for child in board.children(task.id) {
                visit(board, child, depth + 1, filter, hide_closed, folded, rows);
            }
            let passes = shown_in_list(task, filter, hide_closed)
                && (!folded_above || board.slot_of(task.id).is_some());
            if rows.len() > at || passes {
                rows.insert(at, (task.id, depth));
            }
        }
        let mut rows = Vec::new();
        for task in self.tasks.iter().filter(|t| t.parent.is_none()) {
            visit(self, task, 0, filter, hide_closed, false, &mut rows);
        }
        rows
    }

    /// How many subtasks, at any depth, a fold keeps out of `rows` (as built
    /// by `list_rows` with no filter).
    pub fn hidden_subtasks(&self, id: Uuid, rows: &[(Uuid, usize)], hide_closed: bool) -> usize {
        self.descendants(id)
            .into_iter()
            .filter(|d| {
                self.task(*d)
                    .is_some_and(|t| shown_in_list(t, "", hide_closed))
                    && !rows.iter().any(|(r, _)| r == d)
            })
            .count()
    }

    /// Whether the comment can move from one task to another, and if so which
    /// comments go with it (see `Task::linked_notes`). One task blocks on one
    /// thing at a time: an open blocker can't join a blocked task, and a
    /// resolved one can't join a task blocked since before it was resolved.
    pub fn can_move_note(&self, from: Uuid, idx: usize, to: Uuid) -> Result<Vec<usize>, MoveError> {
        let source = self.task(from).ok_or(MoveError::NotFound)?;
        let note = source.notes.get(idx).ok_or(MoveError::NotFound)?;
        let target = self.task(to).ok_or(MoveError::NotFound)?;
        if note.action == Some(NoteAction::Closed) {
            return Err(MoveError::CloseNote);
        }
        if matches!(note.next_step, Some(NextStep::Promoted { .. })) {
            return Err(MoveError::Promoted);
        }
        if from == to {
            return Err(MoveError::SameTask);
        }
        if target.done {
            return Err(MoveError::TargetClosed);
        }
        let linked = source.linked_notes(idx);
        if let Some(open) = target.open_blocker() {
            for note in linked.iter().map(|&i| &source.notes[i]) {
                match note.blocker {
                    Some(Blocker::Open) => return Err(MoveError::AlreadyBlocked),
                    Some(Blocker::Resolved { at }) if open.blocked_since() < at => {
                        return Err(MoveError::BlockedBeforeResolved);
                    }
                    _ => {}
                }
            }
        }
        Ok(linked)
    }

    /// Move the comment, and any tied to it, to another task, keeping the
    /// target's comments in time order. Counts as an update of the target
    /// only as of the moved comments' own times.
    pub fn move_note(&mut self, from: Uuid, idx: usize, to: Uuid) -> Result<(), MoveError> {
        let linked = self.can_move_note(from, idx, to)?;
        let source = self.task_mut(from).ok_or(MoveError::NotFound)?;
        let moved: Vec<Note> = linked
            .iter()
            .rev()
            .map(|&i| source.notes.remove(i))
            .collect();
        let target = self.task_mut(to).ok_or(MoveError::NotFound)?;
        for note in moved {
            target.updated_at = target.updated_at.max(note.at);
            target.insert_note(note);
        }
        Ok(())
    }

    /// The task holding a comment, and the comment's index in it.
    pub fn find_note(&self, id: Uuid) -> Option<(Uuid, usize)> {
        self.tasks
            .iter()
            .find_map(|t| t.note_index(id).map(|idx| (t.id, idx)))
    }

    /// A comment and the task holding it.
    pub fn note(&self, id: Uuid) -> Option<(Uuid, &Note)> {
        let (task, idx) = self.find_note(id)?;
        Some((task, &self.task(task)?.notes[idx]))
    }

    pub fn task(&self, id: Uuid) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn task_mut(&mut self, id: Uuid) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    pub fn slot_of(&self, id: Uuid) -> Option<usize> {
        self.slots.iter().position(|s| *s == Some(id))
    }

    pub fn has_free_slot(&self) -> bool {
        self.slots.iter().any(Option::is_none)
    }

    /// Put the task into the first empty slot. Returns false if it is already
    /// focused, doesn't exist, or every slot is taken.
    pub fn focus(&mut self, id: Uuid) -> bool {
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
    pub fn focus_into(&mut self, slot: usize, id: Uuid) {
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

    /// Drop slot and parent references to tasks that no longer exist, and
    /// parent cycles (e.g. a hand-edited file).
    pub fn sanitize(&mut self) {
        for i in 0..SLOT_COUNT {
            if let Some(id) = self.slots[i]
                && self.task(id).is_none()
            {
                self.slots[i] = None;
            }
        }
        // A parent that is gone or makes a cycle turns the task into a root.
        let ids: Vec<Uuid> = self.tasks.iter().map(|t| t.id).collect();
        for id in ids {
            if let Some(p) = self.task(id).and_then(|t| t.parent)
                && (self.task(p).is_none() || self.is_descendant(p, id))
                && let Some(task) = self.task_mut(id)
            {
                task.parent = None;
            }
        }
        // Copied comments share an id; give the copies new ones.
        let mut seen = HashSet::new();
        for note in self.tasks.iter_mut().flat_map(|t| &mut t.notes) {
            if !seen.insert(note.id) {
                note.id = Uuid::new_v4();
            }
        }
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

    fn board_with(count: usize) -> Board {
        let mut b = Board::default();
        for i in 0..count {
            let id = b.add_task(format!("task {i}"), "", None, Utc::now());
            b.task_mut(id).unwrap().id = n(i as u128);
        }
        b
    }

    /// The id `board_with` gives its `i`th task.
    fn n(i: u128) -> Uuid {
        Uuid::from_u128(i)
    }

    #[test]
    fn focus_fills_first_free_slot_until_full() {
        let mut b = board_with(5);
        for id in 0..4 {
            assert!(b.focus(n(id)));
        }
        assert_eq!(b.slots, [Some(n(0)), Some(n(1)), Some(n(2)), Some(n(3))]);
        assert!(!b.has_free_slot());
        assert!(!b.focus(n(4)));
    }

    #[test]
    fn focus_twice_is_noop() {
        let mut b = board_with(2);
        assert!(b.focus(n(0)));
        assert!(!b.focus(n(0)));
        assert_eq!(b.slots, [Some(n(0)), None, None, None]);
    }

    #[test]
    fn unfocus_frees_slot_and_keeps_task() {
        let mut b = board_with(2);
        b.focus(n(0));
        b.focus(n(1));
        b.unfocus(0);
        assert_eq!(b.slots, [None, Some(n(1)), None, None]);
        assert_eq!(b.tasks.len(), 2);
        assert!(b.focus(n(0)));
        assert_eq!(b.slot_of(n(0)), Some(0));
    }

    #[test]
    fn focus_into_replaces_and_moves() {
        let mut b = board_with(3);
        b.focus(n(0));
        b.focus_into(0, n(1));
        assert_eq!(b.slots, [Some(n(1)), None, None, None]);
        b.focus_into(2, n(1));
        assert_eq!(b.slots, [None, None, Some(n(1)), None]);
    }

    #[test]
    fn delete_clears_slot() {
        let mut b = board_with(2);
        b.focus(n(1));
        b.delete_task(n(1));
        assert_eq!(b.slots, [None; SLOT_COUNT]);
        assert!(b.task(n(1)).is_none());
    }

    #[test]
    fn filter_matches_title_and_notes_case_insensitively() {
        let mut t = Task {
            id: n(0),
            title: "Write Report".into(),
            notes: Vec::new(),
            done: false,
            updated_at: Utc::now(),
            created_at: None,
            done_at: None,
            parent: None,
            folded: false,
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
        b.task_mut(n(1)).unwrap().set_done(true, Utc::now());
        let shown = |filter, hide| {
            b.tasks
                .iter()
                .filter(|t| shown_in_list(t, filter, hide))
                .map(|t| t.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(shown("", false), [n(0), n(1)]);
        assert_eq!(shown("", true), [n(0)]);
        assert_eq!(shown("task 1", false), [n(1)]);
        assert_eq!(shown("task 1", true), Vec::<Uuid>::new());
    }

    #[test]
    fn serde_round_trip() {
        let mut b = board_with(3);
        b.focus(n(2));
        b.task_mut(n(0)).unwrap().done = true;
        let json = serde_json::to_string(&b).unwrap();
        let back: Board = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn legacy_string_notes_become_one_comment() {
        let json =
            r#"{"id":"00000000-0000-0000-0000-000000000000","title":"t","notes":"  old notes "}"#;
        let t: Task = serde_json::from_str(json).unwrap();
        assert_eq!(t.notes.len(), 1);
        assert_eq!(t.notes[0].text, "old notes");

        let json = r#"{"id":"00000000-0000-0000-0000-000000000000","title":"t","notes":""}"#;
        let t: Task = serde_json::from_str(json).unwrap();
        assert!(t.notes.is_empty());
    }

    #[test]
    fn post_touches_and_skips_blank() {
        let mut b = board_with(1);
        let t = b.task_mut(n(0)).unwrap();
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
        let t = b.task_mut(n(0)).unwrap();
        assert_eq!(t.state(), TaskState::Active);
        t.post("waiting on new input case", MessageKind::Block, now);
        t.post("plain comment", MessageKind::Comment, now);
        t.post("also need access", MessageKind::Comment, now);
        assert_eq!(t.state(), TaskState::Blocked);
        // One blocker at a time.
        assert!(!t.can_block());
        t.set_blocking(2, now);
        assert_eq!(t.notes[2].blocker, None);

        t.resolve(0, now);
        assert_eq!(t.state(), TaskState::Active);
        assert_eq!(t.notes[0].blocker, Some(Blocker::Resolved { at: now }));
        t.set_blocking(2, now);
        assert_eq!(t.state(), TaskState::Blocked);
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
    fn next_steps_are_ticked_off_directly() {
        let mut b = board_with(1);
        let t0 = Utc::now();
        let h = TimeDelta::hours;
        let t = b.task_mut(n(0)).unwrap();
        t.post("call Bob", MessageKind::Next, t0);
        t.post("write it up", MessageKind::Next, t0 + h(1));
        assert_eq!(t.state(), TaskState::Ready);
        assert_eq!(t.notes[0].next_step, Some(NextStep::Open { since: t0 }));
        // Starred, like blockers.
        assert!(t.notes[0].starred);
        assert_eq!(t.open_steps().count(), 2);

        t.toggle_step(0, t0 + h(2));
        assert_eq!(
            t.notes[0].next_step,
            Some(NextStep::Done {
                since: t0,
                at: t0 + h(2)
            })
        );
        assert_eq!(t.updated_at, t0 + h(2));
        assert_eq!(t.state(), TaskState::Ready);
        t.toggle_step(1, t0 + h(3));
        assert_eq!(t.state(), TaskState::Active);
        // Reopening keeps when it became a step.
        t.toggle_step(0, t0 + h(4));
        assert_eq!(t.notes[0].next_step, Some(NextStep::Open { since: t0 }));

        // Doesn't count as an update.
        t.unmark_next_step(0);
        assert_eq!(t.notes[0].next_step, None);
        assert_eq!(t.updated_at, t0 + h(4));
        t.toggle_step(0, t0 + h(5)); // not a step: stays untouched
        assert_eq!(t.notes[0].next_step, None);
        t.mark_next_step(0, t0 + h(5));
        assert_eq!(
            t.notes[0].next_step,
            Some(NextStep::Open { since: t0 + h(5) })
        );
    }

    #[test]
    fn blocked_wins_over_ready_and_done_over_both() {
        let mut b = board_with(1);
        let now = Utc::now();
        let t = b.task_mut(n(0)).unwrap();
        t.post("call Bob", MessageKind::Next, now);
        t.post("waiting on Alice", MessageKind::Block, now);
        assert_eq!(t.state(), TaskState::Blocked);
        t.resolve(1, now);
        assert_eq!(t.state(), TaskState::Ready);
        t.set_done(true, now);
        assert_eq!(t.state(), TaskState::Done);

        // Subtasks blocking the parent outweigh its own steps.
        let mut b = tree();
        b.task_mut(n(0))
            .unwrap()
            .post("plan", MessageKind::Next, now);
        assert_eq!(b.state(n(0)), TaskState::Ready);
        for id in [1, 2] {
            b.task_mut(n(id))
                .unwrap()
                .post("wait", MessageKind::Block, now);
        }
        assert_eq!(b.state(n(0)), TaskState::Blocked);
        // A subtask's step doesn't make the parent ready.
        let mut b = tree();
        b.task_mut(n(1))
            .unwrap()
            .post("plan", MessageKind::Next, now);
        assert_eq!(b.state(n(0)), TaskState::Active);
    }

    #[test]
    fn a_comment_is_a_blocker_or_a_step_not_both() {
        let mut b = board_with(1);
        let now = Utc::now();
        let t = b.task_mut(n(0)).unwrap();
        t.post("waiting", MessageKind::Block, now);
        t.post("call Bob", MessageKind::Next, now);
        t.mark_next_step(0, now);
        assert_eq!(t.notes[0].next_step, None);
        t.resolve(0, now);
        t.mark_next_step(0, now);
        assert_eq!(t.notes[0].next_step, None);
        t.set_blocking(1, now);
        assert_eq!(t.notes[1].blocker, None);
        // An Unblock message isn't a plain comment either.
        t.set_blocking(0, now);
        t.post("got it", MessageKind::Unblock, now);
        t.mark_next_step(2, now);
        assert_eq!(t.notes[2].next_step, None);
    }

    #[test]
    fn steps_round_trip_and_old_notes_load_without() {
        let mut b = board_with(1);
        let now = Utc::now();
        let t = b.task_mut(n(0)).unwrap();
        t.post("open", MessageKind::Next, now);
        t.post("done", MessageKind::Next, now);
        t.toggle_step(1, now);
        t.post("grows", MessageKind::Next, now);
        b.promote_step(n(0), 2, "grows".into(), "", now);
        let json = serde_json::to_string(&b).unwrap();
        let back: Board = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);

        let json = r#"{"id":"00000000-0000-0000-0000-000000000000","title":"t","notes":[{"at":"2026-10-01T10:00:00Z","text":"x"}]}"#;
        let t: Task = serde_json::from_str(json).unwrap();
        assert_eq!(t.notes[0].next_step, None);
    }

    #[test]
    fn promoting_a_step_makes_a_subtask_with_its_history() {
        let mut b = board_with(1);
        let t0 = Utc::now();
        let h = TimeDelta::hours;
        let t = b.task_mut(n(0)).unwrap();
        t.post(
            "Write the report\nWith the Q3 numbers",
            MessageKind::Comment,
            t0,
        );
        t.mark_next_step(0, t0 + h(1));
        t.post("call Bob", MessageKind::Next, t0 + h(2));

        let id = b
            .promote_step(
                n(0),
                0,
                "Write the report".into(),
                "With the Q3 numbers",
                t0 + h(3),
            )
            .unwrap();
        let sub = b.task(id).unwrap();
        assert_eq!(sub.parent, Some(n(0)));
        assert_eq!(sub.title, "Write the report");
        assert_eq!(sub.created_at, Some(t0));
        assert_eq!(sub.updated_at, t0 + h(1));
        assert_eq!(sub.notes.len(), 1);
        assert_eq!(sub.notes[0].text, "With the Q3 numbers");
        assert_eq!(sub.notes[0].at, t0);
        assert!(sub.notes[0].starred);
        assert_eq!(sub.state(), TaskState::Active);

        let t = b.task(n(0)).unwrap();
        assert_eq!(
            t.notes[0].next_step,
            Some(NextStep::Promoted {
                since: t0 + h(1),
                at: t0 + h(3),
                subtask: id
            })
        );
        assert_eq!(t.updated_at, t0 + h(3));
        // Still ready: "call Bob" is open.
        assert_eq!(b.state(n(0)), TaskState::Ready);
        // Only open steps get promoted, and only once.
        assert_eq!(b.promote_step(n(0), 0, "again".into(), "", t0), None);
        b.task_mut(n(0)).unwrap().toggle_step(1, t0 + h(4));
        assert_eq!(b.promote_step(n(0), 1, "x".into(), "", t0), None);
        assert_eq!(b.state(n(0)), TaskState::Active);
        // The marker can't be unmarked, ticked or moved.
        let t = b.task_mut(n(0)).unwrap();
        t.unmark_next_step(0);
        t.toggle_step(0, t0);
        assert!(matches!(
            t.notes[0].next_step,
            Some(NextStep::Promoted { .. })
        ));
        assert_eq!(b.can_move_note(n(0), 0, id), Err(MoveError::Promoted));
        // A one-line step leaves the subtask without comments.
        b.task_mut(n(0))
            .unwrap()
            .post("ping", MessageKind::Next, t0);
        let id = b.promote_step(n(0), 2, "ping".into(), "", t0).unwrap();
        assert!(b.task(id).unwrap().notes.is_empty());
    }

    #[test]
    fn blockers_round_trip_and_old_notes_load() {
        let mut b = board_with(1);
        let t = b.task_mut(n(0)).unwrap();
        t.post("resolved", MessageKind::Block, Utc::now());
        t.resolve(0, Utc::now());
        t.post("open", MessageKind::Block, Utc::now());
        let json = serde_json::to_string(&b).unwrap();
        let back: Board = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);

        let json = r#"{"id":"00000000-0000-0000-0000-000000000000","title":"t","notes":[{"at":"2026-10-01T10:00:00Z","text":"x"}]}"#;
        let t: Task = serde_json::from_str(json).unwrap();
        assert_eq!(t.notes[0].blocker, None);
        assert_eq!(t.state(), TaskState::Active);
    }

    #[test]
    fn blocking_stars_and_tracks_durations() {
        let mut b = board_with(1);
        let t0 = Utc::now();
        let t = b.task_mut(n(0)).unwrap();
        t.post("posted as blocker", MessageKind::Block, t0);
        t.post("plain", MessageKind::Comment, t0);
        assert!(t.notes[0].starred);
        assert!(!t.notes[1].starred);
        assert_eq!(
            t.notes[0].blocked_for(t0 + TimeDelta::hours(5)),
            Some(TimeDelta::hours(5))
        );

        let later = t0 + TimeDelta::hours(2);
        t.resolve(0, later);
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
    fn unblock_resolves_the_blocker_with_a_message() {
        let mut b = board_with(1);
        let t0 = Utc::now();
        let t = b.task_mut(n(0)).unwrap();
        assert_eq!(
            t.message_kinds(),
            [
                MessageKind::Comment,
                MessageKind::Star,
                MessageKind::Next,
                MessageKind::Block,
                MessageKind::Close
            ]
        );
        t.post("need access", MessageKind::Block, t0);
        // Already blocked: posted as a plain comment.
        t.post("need data", MessageKind::Block, t0 + TimeDelta::hours(1));
        assert_eq!(t.notes[1].blocker, None);
        assert_eq!(
            t.message_kinds(),
            [
                MessageKind::Comment,
                MessageKind::Star,
                MessageKind::Next,
                MessageKind::Unblock,
                MessageKind::Close
            ]
        );

        let later = t0 + TimeDelta::hours(5);
        t.post("got access", MessageKind::Unblock, later);
        assert_eq!(t.notes[0].blocker, Some(Blocker::Resolved { at: later }));
        assert_eq!(t.state(), TaskState::Active);
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

        // Nothing left to resolve: a plain comment.
        t.post("extra", MessageKind::Unblock, later);
        assert_eq!(t.notes[3].action, None);
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
        let t = b.task_mut(n(0)).unwrap();
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
    fn star_posts_a_starred_comment() {
        let mut b = board_with(1);
        let now = Utc::now();
        let t = b.task_mut(n(0)).unwrap();
        t.post("nice idea", MessageKind::Star, now);
        let note = &t.notes[0];
        assert!(note.starred);
        assert_eq!(note.blocker, None);
        assert_eq!(note.action, None);
        assert_eq!(t.state(), TaskState::Active);
    }

    #[test]
    fn close_marks_done_with_a_message() {
        let mut b = board_with(1);
        let now = Utc::now();
        let t = b.task_mut(n(0)).unwrap();
        t.post("   ", MessageKind::Close, now);
        assert!(!t.done);
        t.post("shipped", MessageKind::Close, now);
        assert_eq!(t.state(), TaskState::Done);
        assert_eq!(t.done_at, Some(now));
        assert_eq!(t.notes[0].action, Some(NoteAction::Closed));
        assert_eq!(
            t.message_kinds(),
            [
                MessageKind::Comment,
                MessageKind::Star,
                MessageKind::Next,
                MessageKind::Block
            ]
        );

        let json = serde_json::to_string(&b).unwrap();
        let back: Board = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn set_done_records_time() {
        let mut b = board_with(1);
        let now = Utc::now();
        let t = b.task_mut(n(0)).unwrap();
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
    fn sanitize_drops_dangling_slots() {
        let mut b = board_with(2);
        b.slots[3] = Some(n(99));
        b.sanitize();
        assert_eq!(b.slots[3], None);
    }

    #[test]
    fn new_tasks_sort_in_creation_order() {
        let mut b = Board::default();
        let ids: Vec<Uuid> = (0..50)
            .map(|i| b.add_task(format!("{i}"), "", None, Utc::now()))
            .collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(sorted, ids);
    }

    /// 0 ─┬─ 1 ── 3
    ///    └─ 2
    /// 4
    fn tree() -> Board {
        let mut b = board_with(5);
        b.set_parent(n(1), Some(n(0)));
        b.set_parent(n(2), Some(n(0)));
        b.set_parent(n(3), Some(n(1)));
        b
    }

    #[test]
    fn set_parent_refuses_cycles_and_unknown_tasks() {
        let mut b = tree();
        assert!(!b.set_parent(n(0), Some(n(3))));
        assert!(!b.set_parent(n(0), Some(n(0))));
        assert!(!b.set_parent(n(4), Some(n(99))));
        assert!(!b.set_parent(n(99), Some(n(0))));
        assert!(b.is_descendant(n(3), n(0)));
        assert!(!b.is_descendant(n(0), n(3)));
        assert_eq!(b.descendants(n(0)).len(), 3);
        assert!(b.set_parent(n(3), None));
        assert_eq!(b.task(n(3)).unwrap().parent, None);
    }

    #[test]
    fn subtasks_all_blocked_block_the_parent() {
        let now = Utc::now();
        let mut b = tree();
        assert_eq!(b.state(n(0)), TaskState::Active);
        b.task_mut(n(3)).unwrap().post("x", MessageKind::Block, now);
        // 1 is blocked through 3; 2 is still active.
        assert_eq!(b.state(n(1)), TaskState::Blocked);
        assert!(b.blocked_by_subtasks(n(1)));
        assert_eq!(b.state(n(0)), TaskState::Active);
        // Done subtasks don't count.
        b.task_mut(n(2)).unwrap().set_done(true, now);
        assert_eq!(b.state(n(0)), TaskState::Blocked);
        assert!(b.blocked_by_subtasks(n(0)));
        // No open subtasks at all: not blocked by them.
        b.task_mut(n(1)).unwrap().set_done(true, now);
        assert_eq!(b.state(n(0)), TaskState::Active);
        assert!(!b.blocked_by_subtasks(n(4)));
        // A done parent stays done.
        b.task_mut(n(1)).unwrap().set_done(false, now);
        b.task_mut(n(0)).unwrap().set_done(true, now);
        assert_eq!(b.state(n(0)), TaskState::Done);
    }

    #[test]
    fn own_blocker_blocks_a_parent_with_active_subtasks() {
        let mut b = tree();
        b.task_mut(n(0))
            .unwrap()
            .post("x", MessageKind::Block, Utc::now());
        assert_eq!(b.state(n(0)), TaskState::Blocked);
        assert!(!b.blocked_by_subtasks(n(0)));
    }

    #[test]
    fn blocked_since_takes_the_last_subtask_or_the_own_blocker() {
        let t0 = Utc::now();
        let h = TimeDelta::hours;
        let mut b = tree();
        assert_eq!(b.blocked_since(n(0)), None);
        b.task_mut(n(3)).unwrap().post("a", MessageKind::Block, t0);
        b.task_mut(n(2))
            .unwrap()
            .post("b", MessageKind::Block, t0 + h(2));
        // Through 1 (via 3, since t0) and 2 (since t0+2h): the later one.
        assert_eq!(b.blocked_since(n(1)), Some(t0));
        assert_eq!(b.blocked_since(n(0)), Some(t0 + h(2)));
        assert_eq!(b.blocked_for(n(0), t0 + h(5)), Some(h(3)));
        // An own blocker from before that wins.
        b.task_mut(n(0))
            .unwrap()
            .post("c", MessageKind::Block, t0 + h(1));
        assert_eq!(b.blocked_since(n(0)), Some(t0 + h(1)));

        // Blocked once the last unblocked subtask was closed.
        let mut b = tree();
        b.task_mut(n(3)).unwrap().post("a", MessageKind::Block, t0);
        b.task_mut(n(2)).unwrap().set_done(true, t0 + h(4));
        assert_eq!(b.blocked_since(n(0)), Some(t0 + h(4)));
        b.task_mut(n(0)).unwrap().set_done(true, t0 + h(5));
        assert_eq!(b.blocked_since(n(0)), None);
    }

    #[test]
    fn delete_takes_subtasks_and_their_slots() {
        let mut b = tree();
        b.focus(n(3));
        b.focus(n(4));
        b.delete_task(n(1));
        let ids: Vec<_> = b.tasks.iter().map(|t| t.id).collect();
        assert_eq!(ids, [n(0), n(2), n(4)]);
        assert_eq!(b.slots, [None, Some(n(4)), None, None]);
    }

    #[test]
    fn sanitize_gives_copied_notes_new_ids() {
        let mut b = board_with(1);
        let t = &mut b.tasks[0];
        t.post("a", MessageKind::Comment, Utc::now());
        let copy = t.notes[0].clone();
        t.notes.push(copy);
        b.sanitize();
        let notes = &b.tasks[0].notes;
        assert_ne!(notes[0].id, notes[1].id);
        assert_eq!(b.find_note(notes[1].id), Some((b.tasks[0].id, 1)));
    }

    #[test]
    fn sanitize_drops_dangling_and_cyclic_parents() {
        let mut b = board_with(4);
        b.task_mut(n(0)).unwrap().parent = Some(n(1));
        b.task_mut(n(1)).unwrap().parent = Some(n(0));
        b.task_mut(n(2)).unwrap().parent = Some(n(99));
        b.task_mut(n(3)).unwrap().parent = Some(n(1));
        b.sanitize();
        let parents: Vec<_> = b.tasks.iter().map(|t| t.parent).collect();
        assert_eq!(parents, [None, Some(n(0)), None, Some(n(1))]);
    }

    #[test]
    fn list_rows_nest_and_keep_ancestors_of_matches() {
        let mut b = tree();
        assert_eq!(
            b.list_rows("", false),
            [(n(0), 0), (n(1), 1), (n(3), 2), (n(2), 1), (n(4), 0)]
        );
        assert_eq!(
            b.list_rows("task 3", false),
            [(n(0), 0), (n(1), 1), (n(3), 2)]
        );
        b.task_mut(n(0)).unwrap().set_done(true, Utc::now());
        b.task_mut(n(2)).unwrap().set_done(true, Utc::now());
        // 0 is closed but stays to hold its open subtasks.
        assert_eq!(
            b.list_rows("", true),
            [(n(0), 0), (n(1), 1), (n(3), 2), (n(4), 0)]
        );
    }

    #[test]
    fn folding_hides_subtasks_except_those_on_the_board() {
        let mut b = tree();
        b.task_mut(n(0)).unwrap().toggle_fold();
        let rows = b.list_rows("", false);
        assert_eq!(rows, [(n(0), 0), (n(4), 0)]);
        assert_eq!(b.hidden_subtasks(n(0), &rows, false), 3);
        // A focused subtask stays, with the path down to it.
        b.focus(n(3));
        let rows = b.list_rows("", false);
        assert_eq!(rows, [(n(0), 0), (n(1), 1), (n(3), 2), (n(4), 0)]);
        assert_eq!(b.hidden_subtasks(n(0), &rows, false), 1);
        // Folding a leaf changes nothing.
        b.task_mut(n(4)).unwrap().toggle_fold();
        assert_eq!(b.list_rows("", false), rows);
    }

    #[test]
    fn filtering_ignores_folds() {
        let mut b = tree();
        b.task_mut(n(0)).unwrap().toggle_fold();
        assert_eq!(b.list_rows("task 2", false), [(n(0), 0), (n(2), 1)]);
        assert_eq!(b.list_rows("  ", false), [(n(0), 0), (n(4), 0)]);
    }

    #[test]
    fn folded_round_trips_and_old_files_load_unfolded() {
        let mut b = tree();
        b.task_mut(n(1)).unwrap().toggle_fold();
        let json = serde_json::to_string(&b).unwrap();
        let back: Board = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
        let t: Task =
            serde_json::from_str(r#"{"id":"00000000-0000-0000-0000-000000000000","title":"t"}"#)
                .unwrap();
        assert!(!t.folded);
    }

    #[test]
    fn parent_round_trips_and_old_files_load_as_roots() {
        let b = tree();
        let json = serde_json::to_string(&b).unwrap();
        let back: Board = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
        let t: Task =
            serde_json::from_str(r#"{"id":"00000000-0000-0000-0000-000000000000","title":"t"}"#)
                .unwrap();
        assert_eq!(t.parent, None);
    }

    fn texts(b: &Board, id: Uuid) -> Vec<&str> {
        b.task(id)
            .unwrap()
            .notes
            .iter()
            .map(|n| n.text.as_str())
            .collect()
    }

    #[test]
    fn move_note_lands_in_time_order_and_touches_target() {
        let t0 = Utc::now();
        let h = TimeDelta::hours;
        let mut b = board_with(2);
        b.task_mut(n(0))
            .unwrap()
            .post("wrong task", MessageKind::Comment, t0 + h(2));
        let target = b.task_mut(n(1)).unwrap();
        target.post("before", MessageKind::Comment, t0 + h(1));
        target.post("after", MessageKind::Comment, t0 + h(3));
        target.updated_at = t0;
        assert_eq!(b.move_note(n(0), 0, n(1)), Ok(()));
        assert!(texts(&b, n(0)).is_empty());
        assert_eq!(texts(&b, n(1)), ["before", "wrong task", "after"]);
        assert_eq!(b.task(n(1)).unwrap().updated_at, t0 + h(2));
    }

    #[test]
    fn a_blocker_and_its_resolve_message_move_together() {
        let t0 = Utc::now();
        let h = TimeDelta::hours;
        let mut b = board_with(2);
        let t = b.task_mut(n(0)).unwrap();
        t.post("need access", MessageKind::Block, t0);
        t.post("plain", MessageKind::Comment, t0 + h(1));
        t.post("got it", MessageKind::Unblock, t0 + h(2));
        assert_eq!(t.linked_notes(0), [0, 2]);
        assert_eq!(t.linked_notes(2), [0, 2]);
        assert_eq!(t.linked_notes(1), [1]);
        assert_eq!(b.move_note(n(0), 2, n(1)), Ok(()));
        assert_eq!(texts(&b, n(0)), ["plain"]);
        assert_eq!(texts(&b, n(1)), ["need access", "got it"]);

        // Resolved with the button: no message to take along.
        let t = b.task_mut(n(0)).unwrap();
        t.set_blocking(0, t0 + h(3));
        t.resolve(0, t0 + h(4));
        assert_eq!(t.linked_notes(0), [0]);
    }

    #[test]
    fn moving_an_open_blocker_needs_an_unblocked_target() {
        let now = Utc::now();
        let mut b = board_with(3);
        b.task_mut(n(0)).unwrap().post("a", MessageKind::Block, now);
        b.task_mut(n(1)).unwrap().post("b", MessageKind::Block, now);
        assert_eq!(b.move_note(n(0), 0, n(1)), Err(MoveError::AlreadyBlocked));
        assert_eq!(b.move_note(n(0), 0, n(2)), Ok(()));
        assert_eq!(b.state(n(0)), TaskState::Active);
        assert_eq!(b.state(n(2)), TaskState::Blocked);
    }

    #[test]
    fn a_resolved_blocker_cannot_overlap_the_targets_open_one() {
        let t0 = Utc::now();
        let h = TimeDelta::hours;
        let mut b = board_with(3);
        let t = b.task_mut(n(0)).unwrap();
        t.post("old", MessageKind::Block, t0);
        t.post("done", MessageKind::Unblock, t0 + h(2));
        // Blocked since before "old" was resolved, and since after.
        b.task_mut(n(1))
            .unwrap()
            .post("x", MessageKind::Block, t0 + h(1));
        b.task_mut(n(2))
            .unwrap()
            .post("y", MessageKind::Block, t0 + h(3));
        assert_eq!(
            b.can_move_note(n(0), 0, n(1)),
            Err(MoveError::BlockedBeforeResolved)
        );
        assert_eq!(
            b.can_move_note(n(0), 1, n(1)),
            Err(MoveError::BlockedBeforeResolved)
        );
        assert_eq!(b.move_note(n(0), 1, n(2)), Ok(()));
        assert_eq!(texts(&b, n(2)), ["old", "done", "y"]);
    }

    #[test]
    fn closed_targets_close_notes_and_the_same_task_are_refused() {
        let now = Utc::now();
        let mut b = board_with(3);
        b.task_mut(n(0))
            .unwrap()
            .post("c", MessageKind::Comment, now);
        b.task_mut(n(1)).unwrap().set_done(true, now);
        b.task_mut(n(2))
            .unwrap()
            .post("shipped", MessageKind::Close, now);
        assert_eq!(b.can_move_note(n(0), 0, n(0)), Err(MoveError::SameTask));
        assert_eq!(b.can_move_note(n(0), 0, n(1)), Err(MoveError::TargetClosed));
        assert_eq!(b.can_move_note(n(2), 0, n(0)), Err(MoveError::CloseNote));
        assert_eq!(b.can_move_note(n(0), 5, n(2)), Err(MoveError::NotFound));
        assert_eq!(b.can_move_note(n(0), 0, n(99)), Err(MoveError::NotFound));
    }

    #[test]
    fn plain_text_and_escaped_slashes_post_as_is() {
        let t = board_with(1).tasks.remove(0);
        let p = t.parse_input("hello /block");
        assert_eq!(p.result, Ok(Submit::Post(None, "hello /block")));
        assert_eq!(p.token_len, None);
        let p = t.parse_input("//block me");
        assert_eq!(p.result, Ok(Submit::Post(None, "/block me")));
        assert_eq!(p.token_len, None);
    }

    #[test]
    fn commands_alone_switch_and_with_text_post() {
        let mut t = board_with(1).tasks.remove(0);
        let p = t.parse_input("/block");
        assert_eq!(p.result, Ok(Submit::SetKind(MessageKind::Block)));
        assert_eq!(p.token_len, Some(6));
        assert_eq!(p.preview(), None);
        assert_eq!(t.parse_input("/block ").preview(), Some(Command::Block));
        let p = t.parse_input("/block  need access\nfrom IT ");
        assert_eq!(
            p.result,
            Ok(Submit::Post(
                Some(MessageKind::Block),
                "need access\nfrom IT"
            ))
        );
        assert_eq!(p.preview(), Some(Command::Block));
        assert_eq!(
            t.parse_input("/close done").result,
            Ok(Submit::Post(Some(MessageKind::Close), "done"))
        );
        assert_eq!(
            t.parse_input("/sub-task Write tests").result,
            Ok(Submit::NewSubtask("Write tests"))
        );
        assert_eq!(t.parse_input("/subtask").result, Ok(Submit::NewSubtask("")));
        assert_eq!(
            t.parse_input("/star").result,
            Ok(Submit::SetKind(MessageKind::Star))
        );
        let p = t.parse_input("/star nice idea");
        assert_eq!(
            p.result,
            Ok(Submit::Post(Some(MessageKind::Star), "nice idea"))
        );
        assert_eq!(p.preview(), Some(Command::Star));

        t.post("x", MessageKind::Block, Utc::now());
        assert_eq!(
            t.parse_input("/block").result,
            Err(CommandError::AlreadyBlocked)
        );
        assert_eq!(
            t.parse_input("/unblock got it").result,
            Ok(Submit::Post(Some(MessageKind::Unblock), "got it"))
        );
        assert_eq!(
            t.parse_input("/release got it").result,
            Ok(Submit::Post(Some(MessageKind::Unblock), "got it"))
        );
    }

    #[test]
    fn commands_that_cannot_apply_are_errors() {
        let mut t = board_with(1).tasks.remove(0);
        let p = t.parse_input("/unblock got it");
        assert_eq!(p.result, Err(CommandError::NothingToUnblock));
        assert_eq!(p.preview(), None);
        assert_eq!(
            t.parse_input("/release").result,
            Err(CommandError::NothingToUnblock)
        );
        t.post("x", MessageKind::Block, Utc::now());
        assert_eq!(
            t.parse_input("/block another").result,
            Err(CommandError::AlreadyBlocked)
        );
        assert_eq!(
            t.parse_input("/block").result,
            Err(CommandError::AlreadyBlocked)
        );
        t.set_done(true, Utc::now());
        assert_eq!(
            t.parse_input("/close").result,
            Err(CommandError::AlreadyClosed)
        );
    }

    #[test]
    fn unknown_commands_and_prefixes() {
        let t = board_with(1).tasks.remove(0);
        let p = t.parse_input("/blo");
        assert!(p.partial);
        assert_eq!(p.result, Err(CommandError::Unknown("/blo".into())));
        let p = t.parse_input("/blockx y");
        assert!(!p.partial);
        assert_eq!(p.token_len, Some(7));
        assert_eq!(
            p.result.unwrap_err().to_string(),
            "Unknown command /blockx. Try /next, /block, /unblock, /close, /star or /sub-task"
        );
        assert!(t.parse_input("/").partial);
        assert!(t.parse_input("/nex").partial);
        assert_eq!(
            t.parse_input("/next call Bob").result,
            Ok(Submit::Post(Some(MessageKind::Next), "call Bob"))
        );
    }
}
