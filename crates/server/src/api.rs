//! The boards kept by the server, one per user: the rules of
//! `its4tasks_core::api`, plus saving after every change and keeping each
//! user to their own board.

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use its4tasks_core::api::{ApiError, Change, Outcome, apply_change};
use its4tasks_core::model::Board;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::storage::{Store, User};

/// Why a request failed.
#[derive(Debug)]
pub enum Failure {
    /// The board's rules, or ownership, refuse it.
    Api(ApiError),
    /// The database failed.
    Db(DbErr),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Failure::Api(e) => e.fmt(f),
            Failure::Db(e) => write!(f, "database: {e}"),
        }
    }
}

impl From<DbErr> for Failure {
    fn from(e: DbErr) -> Self {
        Failure::Db(e)
    }
}

impl From<ApiError> for Failure {
    fn from(e: ApiError) -> Self {
        Failure::Api(e)
    }
}

pub struct Api {
    /// `None` for boards that aren't saved anywhere.
    store: Option<Store>,
    /// Users seen so far, by `(iss, sub)`. Without a store, the only record.
    users: HashMap<(String, String), User>,
    /// The boards of the users seen so far.
    boards: HashMap<Uuid, Board>,
}

impl Api {
    /// The boards in the database at `url` (see `Store::connect`).
    pub async fn connect(url: &str) -> Result<Api, DbErr> {
        Ok(Api {
            store: Some(Store::connect(url).await?),
            users: HashMap::new(),
            boards: HashMap::new(),
        })
    }

    /// Boards that aren't saved anywhere.
    pub fn new() -> Api {
        Api {
            store: None,
            users: HashMap::new(),
            boards: HashMap::new(),
        }
    }

    /// The user `iss` knows as `sub`, made now if new. New users start
    /// without a board, until they start one or import one.
    pub async fn sign_in(&mut self, iss: &str, sub: &str) -> Result<User, DbErr> {
        let key = (iss.to_owned(), sub.to_owned());
        if let Some(user) = self.users.get(&key) {
            return Ok(user.clone());
        }
        let user = match &self.store {
            Some(store) => store.user(iss, sub).await?.0,
            None => User {
                id: Uuid::now_v7(),
                iss: iss.to_owned(),
                sub: sub.to_owned(),
                started: false,
            },
        };
        self.users.insert(key, user.clone());
        Ok(user)
    }

    /// The user as they are now: `started` changes with their first change.
    fn current(&self, user: &User) -> User {
        self.users
            .get(&(user.iss.clone(), user.sub.clone()))
            .cloned()
            .unwrap_or_else(|| user.clone())
    }

    /// Whether the user has started a board, and the board.
    pub async fn board(&mut self, user: &User) -> Result<(bool, &Board), Failure> {
        let started = self.current(user).started;
        Ok((started, self.load(user.id).await?))
    }

    async fn load(&mut self, user: Uuid) -> Result<&mut Board, DbErr> {
        if !self.boards.contains_key(&user) {
            let board = match &mut self.store {
                Some(store) => store.load(user).await?,
                None => Board::default(),
            };
            self.boards.insert(user, board);
        }
        Ok(self.boards.get_mut(&user).expect("just loaded"))
    }

    /// Applies the change to the user's board and saves it.
    pub async fn apply(&mut self, user: &User, change: Change) -> Result<Outcome, Failure> {
        self.apply_at(user, change, its4tasks_core::api::now())
            .await
    }

    async fn apply_at(
        &mut self,
        user: &User,
        change: Change,
        now: DateTime<Utc>,
    ) -> Result<Outcome, Failure> {
        let board = self.load(user.id).await?;
        let outcome = match apply_change(board, change, now) {
            Ok(outcome) => outcome,
            Err(e) => return Err(self.whose(user, e).await?.into()),
        };
        if let Some(store) = &mut self.store {
            let board = &self.boards[&user.id];
            store.save(user.id, board).await?;
        }
        let user = self.current(user);
        if !user.started {
            if let Some(store) = &self.store {
                store.set_started(user.id).await?;
            }
            let key = (user.iss.clone(), user.sub.clone());
            self.users.insert(
                key,
                User {
                    started: true,
                    ..user
                },
            );
        }
        Ok(outcome)
    }

    /// A task or comment that isn't on the user's board but is on someone
    /// else's is forbidden rather than unknown.
    async fn whose(&self, user: &User, error: ApiError) -> Result<ApiError, DbErr> {
        let owner = match error {
            ApiError::UnknownTask(id) => self.owner_of(id, true).await?,
            ApiError::UnknownNote(id) => self.owner_of(id, false).await?,
            _ => None,
        };
        Ok(match (owner, error) {
            (Some(owner), ApiError::UnknownTask(id) | ApiError::UnknownNote(id))
                if owner != user.id =>
            {
                ApiError::Forbidden(id)
            }
            _ => error,
        })
    }

    async fn owner_of(&self, id: Uuid, task: bool) -> Result<Option<Uuid>, DbErr> {
        if let Some(store) = &self.store {
            return if task {
                store.owner_of_task(id).await
            } else {
                store.owner_of_note(id).await
            };
        }
        Ok(self.boards.iter().find_map(|(owner, board)| {
            let found = if task {
                board.task(id).is_some()
            } else {
                board.find_note(id).is_some()
            };
            found.then_some(*owner)
        }))
    }
}

impl Default for Api {
    fn default() -> Self {
        Api::new()
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use its4tasks_core::model::{MessageKind, MoveError};

    use super::*;

    async fn alice(api: &mut Api) -> User {
        api.sign_in("https://auth.test", "alice").await.unwrap()
    }

    async fn add(api: &mut Api, user: &User, title: &str, focus: bool) -> Uuid {
        let change = Change::add_task(title.into(), String::new(), None, focus);
        match api.apply(user, change).await {
            Ok(Outcome::Created(id)) => id,
            other => panic!("expected a new task, got {other:?}"),
        }
    }

    fn api_error(result: Result<Outcome, Failure>) -> ApiError {
        match result {
            Err(Failure::Api(e)) => e,
            other => panic!("expected an ApiError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn add_task_can_focus_it() {
        let mut api = Api::new();
        let me = alice(&mut api).await;
        let a = add(&mut api, &me, "a", true).await;
        let b = add(&mut api, &me, "b", false).await;
        let (_, board) = api.board(&me).await.unwrap();
        assert_eq!(board.slot_of(a), Some(0));
        assert_eq!(board.slot_of(b), None);
    }

    #[tokio::test]
    async fn rename_touches_the_task() {
        let mut api = Api::new();
        let me = alice(&mut api).await;
        let id = add(&mut api, &me, "a", false).await;
        let later = Utc::now() + TimeDelta::hours(1);
        let rename = Change::Rename {
            task: id,
            title: "b".into(),
        };
        assert!(matches!(
            api.apply_at(&me, rename, later).await,
            Ok(Outcome::Done)
        ));
        let (_, board) = api.board(&me).await.unwrap();
        let t = board.task(id).unwrap();
        assert_eq!((t.title.as_str(), t.updated_at), ("b", later));
    }

    #[tokio::test]
    async fn reports_why_a_change_failed() {
        let mut api = Api::new();
        let me = alice(&mut api).await;
        let a = add(&mut api, &me, "a", false).await;
        let b = add(&mut api, &me, "b", false).await;
        let post = Change::post(a, "hi".into(), MessageKind::Comment);
        api.apply(&me, post).await.unwrap();
        let done = Change::SetDone {
            task: b,
            done: true,
        };
        api.apply(&me, done).await.unwrap();
        let note = api.board(&me).await.unwrap().1.task(a).unwrap().notes[0].id;
        let moved = Change::MoveNote { note, to: b };
        assert_eq!(
            api_error(api.apply(&me, moved).await),
            ApiError::Move(MoveError::TargetClosed)
        );
        let gone = Uuid::new_v4();
        assert_eq!(
            api_error(api.apply(&me, Change::ToggleFold { task: gone }).await),
            ApiError::UnknownTask(gone)
        );
        assert_eq!(
            api_error(api.apply(&me, Change::ToggleStar { note: gone }).await),
            ApiError::UnknownNote(gone)
        );
    }

    #[tokio::test]
    async fn taken_ids_are_refused() {
        let mut api = Api::new();
        let me = alice(&mut api).await;
        let a = add(&mut api, &me, "a", false).await;
        let again = Change::AddTask {
            id: a,
            note_id: Uuid::new_v4(),
            title: "b".into(),
            notes: String::new(),
            parent: None,
            focus: false,
        };
        assert_eq!(
            api_error(api.apply(&me, again).await),
            ApiError::Duplicate(a)
        );
        let post = Change::post(a, "hi".into(), MessageKind::Comment);
        api.apply(&me, post.clone()).await.unwrap();
        // The same post sent twice, say after a lost answer.
        let Change::Post { id, .. } = post else {
            unreachable!()
        };
        assert_eq!(
            api_error(api.apply(&me, post).await),
            ApiError::Duplicate(id)
        );
    }

    #[tokio::test]
    async fn users_keep_to_their_own_boards() {
        let mut api = Api::new();
        let me = alice(&mut api).await;
        let bob = api.sign_in("https://auth.test", "bob").await.unwrap();
        assert!(!me.started && !bob.started);
        let a = add(&mut api, &me, "alice's", true).await;
        let post = Change::post(a, "mine".into(), MessageKind::Comment);
        api.apply(&me, post).await.unwrap();
        let note = api.board(&me).await.unwrap().1.task(a).unwrap().notes[0].id;
        assert!(api.board(&me).await.unwrap().0, "a change starts the board");

        let (started, board) = api.board(&bob).await.unwrap();
        assert!(!started);
        assert!(board.tasks.is_empty());
        for change in [
            Change::Rename {
                task: a,
                title: "bob's now".into(),
            },
            Change::DeleteTask { task: a },
            Change::FocusInto { slot: 1, task: a },
            Change::post(a, "hi".into(), MessageKind::Comment),
        ] {
            assert_eq!(
                api_error(api.apply(&bob, change).await),
                ApiError::Forbidden(a)
            );
        }
        assert_eq!(
            api_error(api.apply(&bob, Change::DeleteNote { note }).await),
            ApiError::Forbidden(note)
        );
        let b = add(&mut api, &bob, "bob's", false).await;
        let moved = Change::MoveNote { note, to: b };
        assert_eq!(
            api_error(api.apply(&bob, moved).await),
            ApiError::Forbidden(note)
        );
        let (_, board) = api.board(&me).await.unwrap();
        assert_eq!(board.task(a).unwrap().title, "alice's");
        assert_eq!(board.task(a).unwrap().notes.len(), 1);
    }
}
