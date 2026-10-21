//! Users' boards in a database, SQLite or Postgres, through sea-orm.
//!
//! The server works on in-memory `Board`s, one per user. Saving one compares
//! it with that user's last saved board and writes only what changed, in one
//! transaction. Every row carries its owner's id.

mod convert;
mod diff;
mod entity;
mod migration;

use std::collections::HashMap;

use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectOptions, ConnectionTrait, Database, DatabaseConnection,
    DbErr, EntityTrait, QueryFilter, QueryOrder, TransactionTrait,
};
use sea_orm_migration::MigratorTrait;
use uuid::Uuid;

use diff::{Changes, diff};
use entity::{note, slot, task, user};
use its4tasks_core::model::{Board, Note, SLOT_COUNT};
use migration::Migrator;

/// The authority and subject of the user the native app's local mode signs
/// in as. Boards kept before there were users belong to it.
pub const LOCAL: &str = "local";

/// Someone who signed in, known to the authority `iss` as `sub`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: Uuid,
    pub iss: String,
    pub sub: String,
    /// Chose to start a new board or import one. New users haven't.
    pub started: bool,
}

pub struct Store {
    db: DatabaseConnection,
    /// What the database holds for each user loaded so far, to diff their
    /// next save against.
    saved: HashMap<Uuid, Board>,
}

impl Store {
    /// Connects to the database at `url` (`sqlite://…` or `postgres://…`)
    /// and brings its schema up to date. A SQLite file and its directory
    /// are created if missing.
    pub async fn connect(url: &str) -> Result<Store, DbErr> {
        if let Some(path) = sqlite_path(url)
            && let Some(dir) = std::path::Path::new(path).parent()
        {
            std::fs::create_dir_all(dir)
                .map_err(|e| DbErr::Custom(format!("{}: {e}", dir.display())))?;
        }
        let mut options = ConnectOptions::new(url);
        options.sqlx_logging(false);
        let db = Database::connect(options).await?;
        Migrator::up(&db, None).await?;
        Ok(Store {
            db,
            saved: HashMap::new(),
        })
    }

    /// The user `iss` knows as `sub`, made now if new. True if it was.
    pub async fn user(&self, iss: &str, sub: &str) -> Result<(User, bool), DbErr> {
        if let Some(user) = self.find_user(iss, sub).await? {
            return Ok((user, false));
        }
        let row = user::ActiveModel {
            id: Set(Uuid::now_v7().to_string()),
            iss: Set(iss.to_owned()),
            sub: Set(sub.to_owned()),
            started: Set(false),
            created_at: Set(its4tasks_core::api::now()),
        };
        // Two first requests at once both get here; one insert wins.
        user::Entity::insert(row)
            .on_conflict(
                OnConflict::columns([user::Column::Iss, user::Column::Sub])
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(&self.db)
            .await?;
        let user = self
            .find_user(iss, sub)
            .await?
            .ok_or_else(|| DbErr::Custom(format!("user {iss} {sub} vanished")))?;
        Ok((user, true))
    }

    async fn find_user(&self, iss: &str, sub: &str) -> Result<Option<User>, DbErr> {
        user::Entity::find()
            .filter(user::Column::Iss.eq(iss))
            .filter(user::Column::Sub.eq(sub))
            .one(&self.db)
            .await?
            .map(|row| {
                Ok(User {
                    id: convert::parse_id(&row.id, "user")?,
                    iss: row.iss,
                    sub: row.sub,
                    started: row.started,
                })
            })
            .transpose()
    }

    /// Records that the user chose to start a new board or import one.
    pub async fn set_started(&self, user: Uuid) -> Result<(), DbErr> {
        user::Entity::update_many()
            .col_expr(user::Column::Started, true.into())
            .filter(user::Column::Id.eq(user.to_string()))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    /// The user's saved board, with dangling references dropped.
    pub async fn load(&mut self, user: Uuid) -> Result<Board, DbErr> {
        let saved = read(&self.db, user).await?;
        let mut board = saved.clone();
        self.saved.insert(user, saved);
        board.sanitize();
        Ok(board)
    }

    /// Saves the user's board: what changed since it was loaded or last saved.
    pub async fn save(&mut self, user: Uuid, board: &Board) -> Result<(), DbErr> {
        let saved = match self.saved.get(&user) {
            Some(saved) => saved,
            None => {
                let saved = read(&self.db, user).await?;
                self.saved.entry(user).or_insert(saved)
            }
        };
        let changes = diff(saved, board);
        if changes.is_empty() {
            return Ok(());
        }
        write(&self.db, user, &changes).await?;
        self.saved.insert(user, board.clone());
        Ok(())
    }

    /// One of these task and comment ids that's on a board other than
    /// `owner`'s, if any.
    pub async fn taken_elsewhere(
        &self,
        owner: Uuid,
        tasks: &[Uuid],
        notes: &[Uuid],
    ) -> Result<Option<Uuid>, DbErr> {
        // Few enough at a time for any database's limit on parameters.
        const CHUNK: usize = 500;
        let owner = owner.to_string();
        let ids = |chunk: &[Uuid]| chunk.iter().map(Uuid::to_string).collect::<Vec<_>>();
        for chunk in tasks.chunks(CHUNK) {
            let row = task::Entity::find()
                .filter(task::Column::Id.is_in(ids(chunk)))
                .filter(task::Column::OwnerId.ne(&owner))
                .one(&self.db)
                .await?;
            if let Some(row) = row {
                return convert::parse_id(&row.id, "task").map(Some);
            }
        }
        for chunk in notes.chunks(CHUNK) {
            let row = note::Entity::find()
                .filter(note::Column::Id.is_in(ids(chunk)))
                .filter(note::Column::OwnerId.ne(&owner))
                .one(&self.db)
                .await?;
            if let Some(row) = row {
                return convert::parse_id(&row.id, "comment").map(Some);
            }
        }
        Ok(None)
    }

    /// Whose board the task is on, if it exists.
    pub async fn owner_of_task(&self, id: Uuid) -> Result<Option<Uuid>, DbErr> {
        let row = task::Entity::find_by_id(id.to_string())
            .one(&self.db)
            .await?;
        row.map(|r| convert::parse_id(&r.owner_id, "owner"))
            .transpose()
    }

    /// Whose board the comment is on, if it exists.
    pub async fn owner_of_note(&self, id: Uuid) -> Result<Option<Uuid>, DbErr> {
        let row = note::Entity::find_by_id(id.to_string())
            .one(&self.db)
            .await?;
        row.map(|r| convert::parse_id(&r.owner_id, "owner"))
            .transpose()
    }
}

/// The file in a `sqlite://` URL.
fn sqlite_path(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("sqlite://")?;
    let path = rest.split('?').next().unwrap_or(rest);
    (!path.is_empty() && path != ":memory:").then_some(path)
}

async fn read(db: &impl ConnectionTrait, owner: Uuid) -> Result<Board, DbErr> {
    let owner = owner.to_string();
    let mut notes: HashMap<String, Vec<Note>> = HashMap::new();
    for row in note::Entity::find()
        .filter(note::Column::OwnerId.eq(&owner))
        .order_by_asc(note::Column::TaskId)
        .order_by_asc(note::Column::Position)
        .all(db)
        .await?
    {
        notes
            .entry(row.task_id.clone())
            .or_default()
            .push(convert::note_from_row(row)?);
    }
    // Task ids are UUID v7s, which sort in creation order.
    let tasks = task::Entity::find()
        .filter(task::Column::OwnerId.eq(&owner))
        .order_by_asc(task::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(|row| {
            let notes = notes.remove(&row.id).unwrap_or_default();
            let mut task = convert::task_from_row(row)?;
            task.notes = notes;
            Ok(task)
        })
        .collect::<Result<_, DbErr>>()?;
    let mut slots = [None; SLOT_COUNT];
    for row in slot::Entity::find()
        .filter(slot::Column::OwnerId.eq(&owner))
        .all(db)
        .await?
    {
        if let Some(s) = usize::try_from(row.position)
            .ok()
            .and_then(|i| slots.get_mut(i))
        {
            *s = row
                .task_id
                .map(|id| convert::parse_id(&id, "slot task"))
                .transpose()?;
        }
    }
    Ok(Board { tasks, slots })
}

/// Writes one user's changes. Updates and deletes only ever touch rows that
/// user owns, so a task or comment id from another board can't reach theirs.
async fn write(db: &DatabaseConnection, owner: Uuid, changes: &Changes<'_>) -> Result<(), DbErr> {
    let owner_id = owner.to_string();
    let txn = db.begin().await?;
    for &t in &changes.upserts {
        task::Entity::insert(convert::task_row(owner, t))
            .on_conflict(
                OnConflict::column(task::Column::Id)
                    .update_columns([
                        task::Column::Title,
                        task::Column::Done,
                        task::Column::UpdatedAt,
                        task::Column::CreatedAt,
                        task::Column::DoneAt,
                        task::Column::ParentId,
                        task::Column::Folded,
                    ])
                    .action_and_where(task::Column::OwnerId.eq(&owner_id))
                    .to_owned(),
            )
            .exec_without_returning(&txn)
            .await?;
    }
    for &(task_id, position, n) in &changes.note_upserts {
        note::Entity::insert(convert::note_row(owner, task_id, position, n))
            .on_conflict(
                OnConflict::column(note::Column::Id)
                    .update_columns([
                        note::Column::TaskId,
                        note::Column::Position,
                        note::Column::At,
                        note::Column::Text,
                        note::Column::Starred,
                        note::Column::BlockedAt,
                        note::Column::BlockerState,
                        note::Column::BlockerResolvedAt,
                        note::Column::NextStepState,
                        note::Column::NextStepSince,
                        note::Column::NextStepAt,
                        note::Column::NextStepSubtask,
                        note::Column::ActionKind,
                        note::Column::ActionQuote,
                        note::Column::ActionBlockedSince,
                    ])
                    .action_and_where(note::Column::OwnerId.eq(&owner_id))
                    .to_owned(),
            )
            .exec_without_returning(&txn)
            .await?;
    }
    if !changes.note_deletes.is_empty() {
        note::Entity::delete_many()
            .filter(note::Column::OwnerId.eq(&owner_id))
            .filter(note::Column::Id.is_in(changes.note_deletes.iter().map(Uuid::to_string)))
            .exec(&txn)
            .await?;
    }
    if !changes.deletes.is_empty() {
        task::Entity::delete_many()
            .filter(task::Column::OwnerId.eq(&owner_id))
            .filter(task::Column::Id.is_in(changes.deletes.iter().map(Uuid::to_string)))
            .exec(&txn)
            .await?;
    }
    for &(i, task_id) in &changes.slots {
        slot::Entity::insert(slot::ActiveModel {
            owner_id: Set(owner_id.clone()),
            position: Set(i as i32),
            task_id: Set(task_id.map(|id| id.to_string())),
        })
        .on_conflict(
            OnConflict::columns([slot::Column::OwnerId, slot::Column::Position])
                .update_column(slot::Column::TaskId)
                .to_owned(),
        )
        .exec_without_returning(&txn)
        .await?;
    }
    txn.commit().await
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use tempfile::TempDir;

    use super::*;
    use its4tasks_core::model::{Blocker, NextStep, NoteAction};

    /// A fresh, empty database: a SQLite file, and with `TEST_DATABASE_URL`
    /// (a Postgres server URL) set, a new Postgres database too.
    struct Databases {
        urls: Vec<String>,
        _dir: TempDir,
    }

    async fn databases() -> Databases {
        let dir = tempfile::tempdir().unwrap();
        let mut urls = vec![format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("test.sqlite").display()
        )];
        if let Ok(server) = std::env::var("TEST_DATABASE_URL") {
            let name = format!("its4tasks_test_{}", Uuid::new_v4().simple());
            let admin = Database::connect(&server).await.unwrap();
            admin
                .execute_unprepared(&format!("CREATE DATABASE {name}"))
                .await
                .unwrap();
            let base = server.rsplit_once('/').map_or(server.as_str(), |(b, _)| b);
            urls.push(format!("{base}/{name}"));
        }
        Databases { urls, _dir: dir }
    }

    /// A board using every kind of note state, subtasks and slots.
    fn sample() -> Board {
        let now = its4tasks_core::api::now();
        let ago = |h| now - TimeDelta::hours(h);
        let mut b = Board::default();
        let a = b.add_task(
            "Plan the offsite".into(),
            "first\nwith details",
            None,
            ago(50),
        );
        let s = b.add_task("Book venue".into(), "", Some(a), ago(40));
        let d = b.add_task("Done one".into(), "", None, ago(30));
        let t = b.task_mut(a).unwrap();
        t.folded = true;
        let n = &mut t.notes[0];
        n.blocker = Some(Blocker::Open);
        n.blocked_at = Some(ago(49));
        n.starred = true;
        let mut extra = n.clone();
        extra.text = "resolved one".into();
        extra.blocker = Some(Blocker::Resolved { at: ago(10) });
        t.notes.push(extra.clone());
        extra.blocker = None;
        extra.blocked_at = None;
        extra.action = Some(NoteAction::Unblocked {
            quote: "first".into(),
            blocked_since: ago(49),
        });
        t.notes.push(extra.clone());
        extra.action = None;
        for step in [
            NextStep::Open { since: ago(3) },
            NextStep::Done {
                since: ago(5),
                at: ago(2),
            },
            NextStep::Promoted {
                since: ago(6),
                at: ago(1),
                subtask: s,
            },
        ] {
            extra.next_step = Some(step);
            t.notes.push(extra.clone());
        }
        let t = b.task_mut(d).unwrap();
        t.set_done(true, ago(20));
        let mut closed = extra;
        closed.next_step = None;
        closed.action = Some(NoteAction::Closed);
        t.notes.push(closed);
        b.slots[0] = Some(a);
        b.slots[3] = Some(s);
        // The notes above are copies; give them their own ids.
        b.sanitize();
        b
    }

    async fn reopen(url: &str, user: Uuid) -> Board {
        Store::connect(url).await.unwrap().load(user).await.unwrap()
    }

    async fn alice(store: &Store) -> Uuid {
        store.user("https://auth.test", "alice").await.unwrap().0.id
    }

    #[tokio::test]
    async fn users_are_made_once() {
        for url in databases().await.urls {
            let store = Store::connect(&url).await.unwrap();
            let (a, created) = store.user("https://auth.test", "alice").await.unwrap();
            assert!(created && !a.started);
            store.set_started(a.id).await.unwrap();
            let (again, created) = store.user("https://auth.test", "alice").await.unwrap();
            assert!(!created && again.started);
            assert_eq!(again.id, a.id);
            // The same subject at another authority is someone else.
            let (other, created) = store.user("https://other.test", "alice").await.unwrap();
            assert!(created && other.id != a.id);
        }
    }

    #[tokio::test]
    async fn round_trips_a_board() {
        for url in databases().await.urls {
            let board = sample();
            let mut store = Store::connect(&url).await.unwrap();
            let me = alice(&store).await;
            store.save(me, &board).await.unwrap();
            assert_eq!(reopen(&url, me).await, board, "{url}");
        }
    }

    #[tokio::test]
    async fn saves_later_changes() {
        for url in databases().await.urls {
            let mut board = sample();
            let mut store = Store::connect(&url).await.unwrap();
            let me = alice(&store).await;
            store.save(me, &board).await.unwrap();

            let now = its4tasks_core::api::now();
            board.tasks[0].notes[1].text = "edited".into();
            board.tasks[0].notes.remove(2);
            board.tasks[2].title = "Renamed".into();
            let parent = board.tasks[2].id;
            board.add_task("New subtask".into(), "note", Some(parent), now);
            board.slots[1] = Some(parent);
            store.save(me, &board).await.unwrap();
            assert_eq!(reopen(&url, me).await, board);

            // Deleting a task takes its subtasks, their notes and slots with it.
            board.delete_task(board.tasks[0].id);
            store.save(me, &board).await.unwrap();
            assert_eq!(reopen(&url, me).await, board);
        }
    }

    #[tokio::test]
    async fn moved_comments_keep_their_id() {
        for url in databases().await.urls {
            let mut board = sample();
            let mut store = Store::connect(&url).await.unwrap();
            let me = alice(&store).await;
            store.save(me, &board).await.unwrap();
            let (from, to) = (board.tasks[0].id, board.tasks[1].id);
            let id = board.tasks[0].notes[3].id;
            board.move_note(from, 3, to).unwrap();
            store.save(me, &board).await.unwrap();
            let loaded = reopen(&url, me).await;
            assert_eq!(loaded, board);
            assert_eq!(loaded.find_note(id).map(|(t, _)| t), Some(to));
        }
    }

    #[tokio::test]
    async fn boards_are_kept_apart() {
        for url in databases().await.urls {
            let mut store = Store::connect(&url).await.unwrap();
            let me = alice(&store).await;
            let bob = store.user("https://auth.test", "bob").await.unwrap().0.id;
            let mine = sample();
            store.save(me, &mine).await.unwrap();
            let mut his = Board::default();
            his.add_task("Bob's".into(), "his note", None, its4tasks_core::api::now());
            his.focus(his.tasks[0].id);
            store.save(bob, &his).await.unwrap();

            assert_eq!(reopen(&url, me).await, mine);
            assert_eq!(reopen(&url, bob).await, his);
            let (task, note) = (mine.tasks[0].id, mine.tasks[0].notes[0].id);
            assert_eq!(store.owner_of_task(task).await.unwrap(), Some(me));
            assert_eq!(store.owner_of_note(note).await.unwrap(), Some(me));
            assert_eq!(store.owner_of_task(Uuid::new_v4()).await.unwrap(), None);
            let fresh = Uuid::new_v4();
            let taken = |owner, tasks: Vec<Uuid>, notes: Vec<Uuid>| {
                let store = &store;
                async move { store.taken_elsewhere(owner, &tasks, &notes).await.unwrap() }
            };
            assert_eq!(taken(bob, vec![fresh, task], vec![]).await, Some(task));
            assert_eq!(taken(bob, vec![fresh], vec![note]).await, Some(note));
            assert_eq!(taken(me, vec![task], vec![note]).await, None, "her own");
            assert_eq!(taken(bob, vec![fresh], vec![fresh]).await, None);

            // A board naming someone else's task can't change or delete it.
            let mut sneaky = his.clone();
            let mut stolen = mine.tasks[0].clone();
            stolen.title = "Bob was here".into();
            stolen.notes.clear();
            sneaky.tasks.push(stolen);
            store.save(bob, &sneaky).await.unwrap();
            assert_eq!(reopen(&url, me).await, mine);
            let mut deleting = sneaky.clone();
            deleting.tasks.pop();
            store.save(bob, &deleting).await.unwrap();
            assert_eq!(reopen(&url, me).await, mine);
        }
    }

    #[tokio::test]
    async fn migrating_gives_old_rows_ids_and_the_local_user() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("old.sqlite").display()
        );
        {
            let db = Database::connect(&url).await.unwrap();
            // The schema before comments and tasks had UUIDs.
            Migrator::up(&db, Some(1)).await.unwrap();
            // Task 10 is a subtask of 2 and comes after it, though "10" < "2"
            // as text. One step was promoted to 10, one to a deleted task.
            db.execute_unprepared(
                "INSERT INTO task (id, title, done, updated_at, parent_id, folded)
                     VALUES (2, 'a', false, '2026-10-01 10:00:00+00:00', NULL, false),
                            (10, 'b', false, '2026-10-01 10:00:00+00:00', 2, true);
                 INSERT INTO note (task_id, position, at, text, starred,
                                   next_step_state, next_step_since, next_step_at, next_step_subtask)
                     VALUES (2, 0, '2026-10-01 10:00:00+00:00', 'one', false,
                             'promoted', '2026-10-01 10:00:00+00:00', '2026-10-01 12:00:00+00:00', 10),
                            (2, 1, '2026-10-01 11:00:00+00:00', 'two', true,
                             'promoted', '2026-10-01 10:00:00+00:00', '2026-10-01 12:00:00+00:00', 99);
                 INSERT INTO slot (position, task_id) VALUES (0, 10), (1, NULL);
                 INSERT INTO meta (id, next_id) VALUES (1, 11);",
            )
            .await
            .unwrap();
            db.close().await.unwrap();
        }
        // Everything goes to the local user, who has a board already.
        let store = Store::connect(&url).await.unwrap();
        let (local, created) = store.user(LOCAL, LOCAL).await.unwrap();
        assert!(!created && local.started);
        let board = reopen(&url, local.id).await;
        let titles: Vec<&str> = board.tasks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["a", "b"]);
        let (a, b) = (&board.tasks[0], &board.tasks[1]);
        assert!(a.id.get_version_num() == 7 && b.id.get_version_num() == 7);
        assert_eq!(b.parent, Some(a.id));
        assert!(b.folded);
        assert_eq!(board.slots, [Some(b.id), None, None, None]);

        let notes = &a.notes;
        let texts: Vec<&str> = notes.iter().map(|n| n.text.as_str()).collect();
        assert_eq!(texts, ["one", "two"]);
        assert!(notes[1].starred);
        assert_ne!(notes[0].id, notes[1].id);
        assert!(notes.iter().all(|n| n.id.get_version_num() == 4));
        let subtask = |n: &Note| match n.next_step {
            Some(NextStep::Promoted { subtask, .. }) => subtask,
            other => panic!("not promoted: {other:?}"),
        };
        assert_eq!(subtask(&notes[0]), b.id);
        assert!(board.task(subtask(&notes[1])).is_none());
        // The ids are what's stored, not made up on each load.
        assert_eq!(reopen(&url, local.id).await, board);
    }
}
