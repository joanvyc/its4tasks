//! The board kept by the server: the rules of `its4tasks_core::api`, plus
//! saving to the database after every change.

use chrono::{DateTime, Utc};
use its4tasks_core::api::{ApiError, Change, Outcome, apply_change};
use its4tasks_core::model::Board;
use sea_orm::DbErr;

use crate::storage::Store;

/// The board and its rules, kept by the server. Clients reach it over HTTP
/// (see `server`).
pub struct Api {
    board: Board,
    /// `None` for a board that isn't saved anywhere.
    store: Option<Store>,
}

impl Api {
    /// The board saved in the data directory.
    pub async fn open() -> Result<Api, DbErr> {
        let store = Store::open().await?;
        Ok(Api {
            board: store.load(),
            store: Some(store),
        })
    }

    /// A board that isn't saved anywhere.
    pub fn new(board: Board) -> Api {
        Api { board, store: None }
    }

    pub fn board(&self) -> &Board {
        &self.board
    }

    pub async fn apply(&mut self, change: Change) -> Result<Outcome, ApiError> {
        self.apply_at(change, Utc::now()).await
    }

    async fn apply_at(&mut self, change: Change, now: DateTime<Utc>) -> Result<Outcome, ApiError> {
        let outcome = apply_change(&mut self.board, change, now)?;
        if let Some(store) = &mut self.store
            && let Err(e) = store.save(&self.board).await
        {
            eprintln!("its4tasks: failed to save: {e}");
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use its4tasks_core::model::{MessageKind, MoveError};
    use uuid::Uuid;

    use super::*;
    async fn add(api: &mut Api, title: &str, focus: bool) -> Uuid {
        let change = Change::add_task(title.into(), String::new(), None, focus);
        match api.apply(change).await {
            Ok(Outcome::Created(id)) => id,
            other => panic!("expected a new task, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn add_task_can_focus_it() {
        let mut api = Api::new(Board::default());
        let a = add(&mut api, "a", true).await;
        let b = add(&mut api, "b", false).await;
        assert_eq!(api.board().slot_of(a), Some(0));
        assert_eq!(api.board().slot_of(b), None);
    }

    #[tokio::test]
    async fn rename_touches_the_task() {
        let mut api = Api::new(Board::default());
        let id = add(&mut api, "a", false).await;
        let later = Utc::now() + TimeDelta::hours(1);
        let rename = Change::Rename {
            task: id,
            title: "b".into(),
        };
        assert_eq!(api.apply_at(rename, later).await, Ok(Outcome::Done));
        let t = api.board().task(id).unwrap();
        assert_eq!((t.title.as_str(), t.updated_at), ("b", later));
    }

    #[tokio::test]
    async fn reports_why_a_change_failed() {
        let mut api = Api::new(Board::default());
        let a = add(&mut api, "a", false).await;
        let b = add(&mut api, "b", false).await;
        let post = Change::post(a, "hi".into(), MessageKind::Comment);
        api.apply(post).await.unwrap();
        api.apply(Change::SetDone {
            task: b,
            done: true,
        })
        .await
        .unwrap();
        let moved = Change::MoveNote {
            note: api.board().task(a).unwrap().notes[0].id,
            to: b,
        };
        assert_eq!(
            api.apply(moved).await,
            Err(ApiError::Move(MoveError::TargetClosed))
        );
        assert_eq!(
            api.apply(Change::ToggleFold {
                task: Uuid::from_u128(99)
            })
            .await,
            Err(ApiError::UnknownTask(Uuid::from_u128(99)))
        );
        let gone = Uuid::new_v4();
        assert_eq!(
            api.apply(Change::ToggleStar { note: gone }).await,
            Err(ApiError::UnknownNote(gone))
        );
    }

    #[tokio::test]
    async fn comments_keep_their_id_when_others_go() {
        let mut api = Api::new(Board::default());
        let a = add(&mut api, "a", false).await;
        for text in ["first", "second"] {
            let post = Change::post(a, text.into(), MessageKind::Comment);
            api.apply(post).await.unwrap();
        }
        let ids: Vec<Uuid> = api
            .board()
            .task(a)
            .unwrap()
            .notes
            .iter()
            .map(|n| n.id)
            .collect();
        // With indices, the edit would have landed on "first"'s old slot.
        api.apply(Change::DeleteNote { note: ids[0] })
            .await
            .unwrap();
        let edit = Change::EditNote {
            note: ids[1],
            text: "second, edited".into(),
        };
        api.apply(edit).await.unwrap();
        let notes = &api.board().task(a).unwrap().notes;
        assert_eq!(notes.len(), 1);
        assert_eq!(
            (notes[0].id, notes[0].text.as_str()),
            (ids[1], "second, edited")
        );
        assert_eq!(
            api.apply(Change::ToggleStar { note: ids[0] }).await,
            Err(ApiError::UnknownNote(ids[0]))
        );
    }

    #[tokio::test]
    async fn taken_ids_are_refused() {
        let mut api = Api::new(Board::default());
        let a = add(&mut api, "a", false).await;
        let again = Change::AddTask {
            id: a,
            note_id: Uuid::new_v4(),
            title: "b".into(),
            notes: String::new(),
            parent: None,
            focus: false,
        };
        assert_eq!(api.apply(again).await, Err(ApiError::Duplicate(a)));
        let post = Change::post(a, "hi".into(), MessageKind::Comment);
        api.apply(post.clone()).await.unwrap();
        // The same post sent twice, say after a lost answer.
        let Change::Post { id, .. } = post else {
            unreachable!()
        };
        assert_eq!(api.apply(post).await, Err(ApiError::Duplicate(id)));
        assert_eq!(api.board().task(a).unwrap().notes.len(), 1);
    }
}
