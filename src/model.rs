use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Deserializer, Serialize};

pub const SLOT_COUNT: usize = 4;

/// How long without updates before a task is shown fully red.
const STALE_AFTER_SECS: f32 = 7.0 * 24.0 * 3600.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub at: DateTime<Utc>,
    pub text: String,
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
}

impl Task {
    pub fn touch(&mut self, now: DateTime<Utc>) {
        self.updated_at = now;
    }

    /// Append a comment. Blank text is ignored.
    pub fn add_note(&mut self, text: &str, now: DateTime<Utc>) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        self.notes.push(Note {
            at: now,
            text: text.to_owned(),
        });
        self.touch(now);
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
        Notes::Legacy(text) => vec![Note {
            at: Utc::now(),
            text: text.trim().to_owned(),
        }],
        Notes::Log(notes) => notes,
    })
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
        };
        task.add_note(first_note, now);
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
        };
        t.add_note("send to Alice", Utc::now());
        assert!(matches_filter(&t, ""));
        assert!(matches_filter(&t, "report"));
        assert!(matches_filter(&t, "  ALICE "));
        assert!(!matches_filter(&t, "bob"));
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
    fn add_note_touches_and_skips_blank() {
        let mut b = board_with(1);
        let t = b.task_mut(0).unwrap();
        let later = t.updated_at + TimeDelta::hours(3);
        t.add_note("   ", later);
        assert!(t.notes.is_empty());
        assert!(t.updated_at < later);
        t.add_note("hi", later);
        assert_eq!(t.notes.len(), 1);
        assert_eq!(t.updated_at, later);
        t.delete_note(5);
        t.delete_note(0);
        assert!(t.notes.is_empty());
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
