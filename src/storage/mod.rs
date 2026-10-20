//! The board in a SQLite database, through sea-orm.
//!
//! The app keeps working on an in-memory `Board`. Saving compares it with
//! the last saved one and writes only what changed, in one transaction.

mod convert;
mod diff;
mod entity;
mod json;
mod migration;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectOptions, ConnectionTrait, Database, DatabaseConnection,
    DbErr, EntityTrait, QueryFilter, QueryOrder, TransactionTrait,
};
use sea_orm_migration::MigratorTrait;
use tokio::runtime::Runtime;
use uuid::Uuid;

use crate::model::{Board, Note, SLOT_COUNT};
use diff::{Changes, diff};
use entity::{note, slot, task};
use migration::Migrator;

const DB_FILE: &str = "board.sqlite";

/// `~/.local/share/its4tasks` on Linux, the platform equivalent elsewhere.
fn data_dir() -> Option<PathBuf> {
    ProjectDirs::from("", "", "its4tasks").map(|d| d.data_dir().to_owned())
}

pub struct Store {
    /// sea-orm is async; the app isn't, so every call blocks on this.
    rt: Runtime,
    db: DatabaseConnection,
    /// What the database holds, to diff the next save against.
    saved: Board,
}

impl Store {
    /// Opens (or creates) the database in the data directory, importing the
    /// old `board.json` the first time.
    pub fn open() -> Result<Store, DbErr> {
        let dir = data_dir()
            .ok_or_else(|| DbErr::Custom("could not determine a data directory".into()))?;
        json::migrate_legacy(&dir.join(json::FILE_NAME));
        Self::open_in(&dir)
    }

    fn open_in(dir: &Path) -> Result<Store, DbErr> {
        fs::create_dir_all(dir).map_err(|e| DbErr::Custom(format!("{}: {e}", dir.display())))?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| DbErr::Custom(e.to_string()))?;
        let mut options =
            ConnectOptions::new(format!("sqlite://{}?mode=rwc", dir.join(DB_FILE).display()));
        options.sqlx_logging(false);
        let (db, saved) = rt.block_on(async {
            let db = Database::connect(options).await?;
            Migrator::up(&db, None).await?;
            let saved = read(&db).await?;
            Ok::<_, DbErr>((db, saved))
        })?;
        let mut store = Store { rt, db, saved };
        let json_path = dir.join(json::FILE_NAME);
        if store.saved == Board::default()
            && let Some(board) = json::read(&json_path)
        {
            store.save(&board)?;
            json::mark_imported(&json_path);
        }
        Ok(store)
    }

    /// The saved board, with dangling references dropped.
    pub fn load(&self) -> Board {
        let mut board = self.saved.clone();
        board.sanitize();
        board
    }

    pub fn save(&mut self, board: &Board) -> Result<(), DbErr> {
        let changes = diff(&self.saved, board);
        if changes.is_empty() {
            return Ok(());
        }
        self.rt.block_on(write(&self.db, &changes))?;
        self.saved = board.clone();
        Ok(())
    }
}

async fn read(db: &impl ConnectionTrait) -> Result<Board, DbErr> {
    let mut notes: HashMap<String, Vec<Note>> = HashMap::new();
    for row in note::Entity::find()
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
    for row in slot::Entity::find().all(db).await? {
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

async fn write(db: &DatabaseConnection, changes: &Changes<'_>) -> Result<(), DbErr> {
    let txn = db.begin().await?;
    for &t in &changes.upserts {
        task::Entity::insert(convert::task_row(t))
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
                    .to_owned(),
            )
            .exec_without_returning(&txn)
            .await?;
    }
    for &(task_id, position, n) in &changes.note_upserts {
        note::Entity::insert(convert::note_row(task_id, position, n))
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
                    .to_owned(),
            )
            .exec_without_returning(&txn)
            .await?;
    }
    if !changes.note_deletes.is_empty() {
        note::Entity::delete_many()
            .filter(note::Column::Id.is_in(changes.note_deletes.iter().map(Uuid::to_string)))
            .exec(&txn)
            .await?;
    }
    if !changes.deletes.is_empty() {
        task::Entity::delete_many()
            .filter(task::Column::Id.is_in(changes.deletes.iter().map(Uuid::to_string)))
            .exec(&txn)
            .await?;
    }
    for &(i, task_id) in &changes.slots {
        slot::Entity::insert(slot::ActiveModel {
            position: Set(i as i32),
            task_id: Set(task_id.map(|id| id.to_string())),
        })
        .on_conflict(
            OnConflict::column(slot::Column::Position)
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
    use chrono::{TimeDelta, Utc};

    use super::*;
    use crate::model::{Blocker, NextStep, NoteAction};

    /// A board using every kind of note state, subtasks and slots.
    fn sample() -> Board {
        let now = Utc::now();
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

    fn reopen(dir: &Path) -> Board {
        Store::open_in(dir).unwrap().load()
    }

    #[test]
    fn round_trips_a_board() {
        let dir = tempfile::tempdir().unwrap();
        let board = sample();
        Store::open_in(dir.path()).unwrap().save(&board).unwrap();
        assert_eq!(reopen(dir.path()), board);
    }

    #[test]
    fn saves_later_changes() {
        let dir = tempfile::tempdir().unwrap();
        let mut board = sample();
        let mut store = Store::open_in(dir.path()).unwrap();
        store.save(&board).unwrap();

        let now = Utc::now();
        board.tasks[0].notes[1].text = "edited".into();
        board.tasks[0].notes.remove(2);
        board.tasks[2].title = "Renamed".into();
        let parent = board.tasks[2].id;
        board.add_task("New subtask".into(), "note", Some(parent), now);
        board.slots[1] = Some(parent);
        store.save(&board).unwrap();
        assert_eq!(reopen(dir.path()), board);

        // Deleting a task takes its subtasks, their notes and slots with it.
        board.delete_task(board.tasks[0].id);
        store.save(&board).unwrap();
        assert_eq!(reopen(dir.path()), board);
    }

    #[test]
    fn moved_comments_keep_their_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut board = sample();
        let mut store = Store::open_in(dir.path()).unwrap();
        store.save(&board).unwrap();
        let (from, to) = (board.tasks[0].id, board.tasks[1].id);
        let id = board.tasks[0].notes[3].id;
        board.move_note(from, 3, to).unwrap();
        store.save(&board).unwrap();
        let loaded = reopen(dir.path());
        assert_eq!(loaded, board);
        assert_eq!(loaded.find_note(id).map(|(t, _)| t), Some(to));
    }

    #[test]
    fn migrating_gives_old_rows_ids() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.path().join(DB_FILE).display());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let db = Database::connect(url).await.unwrap();
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
        });
        let board = reopen(dir.path());
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
        assert_eq!(reopen(dir.path()), board);
    }

    #[test]
    fn imports_the_json_board_once() {
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join(json::FILE_NAME);
        let board = sample();
        fs::write(&json, serde_json::to_string(&board).unwrap()).unwrap();

        assert_eq!(reopen(dir.path()), board);
        assert!(!json.exists());
        assert!(dir.path().join("board.json.imported").exists());

        // A new JSON file doesn't replace a board that's already there.
        fs::write(&json, serde_json::to_string(&Board::default()).unwrap()).unwrap();
        assert_eq!(reopen(dir.path()), board);
        assert!(json.exists());
    }

    #[test]
    fn keeps_an_unreadable_json_board() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(json::FILE_NAME), "{ not json").unwrap();
        assert_eq!(reopen(dir.path()), Board::default());
        assert!(dir.path().join("board.json.corrupt").exists());
    }
}
