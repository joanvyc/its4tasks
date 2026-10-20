//! Work that isn't on the server yet: changes it refused while the user was
//! signed out, and drafts. In the browser it's kept in `localStorage`, per
//! user, so that going to sign in again and coming back loses none of it.
//! Natively there's nowhere to go, and nothing is kept.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::Change;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Unsent {
    /// Refused by the server, so not applied there: safe to send again.
    pub changes: Vec<Change>,
    /// Comment boxes' text, by the task whose card they're on.
    pub comment_drafts: Vec<(Uuid, String)>,
    /// A comment being edited, and its new text.
    pub editing: Option<(Uuid, String)>,
    pub new_task: Option<NewTask>,
}

/// The new-task form's contents.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NewTask {
    pub parent: Option<Uuid>,
    pub from_step: Option<Uuid>,
    pub title: String,
    pub notes: String,
    pub add_to_focus: bool,
}

impl Unsent {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
            && self
                .comment_drafts
                .iter()
                .all(|(_, text)| text.trim().is_empty())
            && self.editing.is_none()
            && self.new_task.is_none()
    }
}

/// The comment boxes' text by task, from the cards' (`slots`) boxes.
pub fn drafts_by_task(slots: &[Option<Uuid>], drafts: &[String]) -> Vec<(Uuid, String)> {
    slots
        .iter()
        .zip(drafts)
        .filter_map(|(task, text)| Some((task.as_ref().copied()?, text.clone())))
        .filter(|(_, text)| !text.trim().is_empty())
        .collect()
}

/// Puts drafts back in the boxes of the cards that show their tasks now.
/// A box that has text keeps it, and drafts for tasks not in focus are
/// dropped.
pub fn restore_drafts(slots: &[Option<Uuid>], drafts: &mut [String], saved: &[(Uuid, String)]) {
    for (task, text) in saved {
        if let Some(slot) = slots.iter().position(|s| *s == Some(*task))
            && let Some(draft) = drafts.get_mut(slot)
            && draft.trim().is_empty()
        {
            *draft = text.clone();
        }
    }
}

fn key(user: Uuid) -> String {
    format!("its4tasks.unsent.{user}")
}

/// Keeps `unsent` for `user`, or forgets what was kept if it's empty.
pub fn save(user: Uuid, unsent: &Unsent) {
    #[cfg(target_arch = "wasm32")]
    if let Some(storage) = storage() {
        let _ = if unsent.is_empty() {
            storage.remove_item(&key(user))
        } else {
            let json = serde_json::to_string(unsent).expect("Unsent serializes");
            storage.set_item(&key(user), &json)
        };
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (key(user), unsent);
}

/// What was kept for `user`, if anything, forgetting it.
pub fn take(user: Uuid) -> Option<Unsent> {
    #[cfg(target_arch = "wasm32")]
    {
        let storage = storage()?;
        let json = storage.get_item(&key(user)).ok()??;
        let _ = storage.remove_item(&key(user));
        serde_json::from_str(&json).ok()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = key(user);
        None
    }
}

#[cfg(target_arch = "wasm32")]
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_follow_their_tasks() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let before = [Some(a), Some(b), None, Some(c)];
        let drafts = [
            "for a".to_owned(),
            "  ".to_owned(),
            "lost".to_owned(),
            "for c".to_owned(),
        ];
        let saved = drafts_by_task(&before, &drafts);
        assert_eq!(
            saved,
            vec![(a, "for a".to_owned()), (c, "for c".to_owned())]
        );

        // Back, with the cards moved around and `a` out of focus.
        let after = [Some(c), None, Some(b), None];
        let mut boxes = [String::new(), String::new(), String::new(), String::new()];
        restore_drafts(&after, &mut boxes, &saved);
        assert_eq!(boxes, ["for c", "", "", ""]);
        // Text typed meanwhile wins.
        let mut boxes = [
            "typed".to_owned(),
            String::new(),
            String::new(),
            String::new(),
        ];
        restore_drafts(&after, &mut boxes, &saved);
        assert_eq!(boxes[0], "typed");
    }

    #[test]
    fn empty_is_nothing_to_keep() {
        let mut unsent = Unsent {
            comment_drafts: vec![(Uuid::new_v4(), " ".into())],
            ..Unsent::default()
        };
        assert!(unsent.is_empty());
        unsent.new_task = Some(NewTask::default());
        assert!(!unsent.is_empty());
        let json = serde_json::to_string(&unsent).unwrap();
        assert_eq!(serde_json::from_str::<Unsent>(&json).unwrap(), unsent);
    }
}
