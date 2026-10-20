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

use crate::model::{Board, Note, SLOT_COUNT};
use diff::{Changes, diff};
use entity::{meta, note, slot, task};
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
    let mut notes: HashMap<i64, Vec<Note>> = HashMap::new();
    for row in note::Entity::find()
        .order_by_asc(note::Column::TaskId)
        .order_by_asc(note::Column::Position)
        .all(db)
        .await?
    {
        notes
            .entry(row.task_id)
            .or_default()
            .push(convert::note_from_row(row)?);
    }
    let tasks = task::Entity::find()
        .order_by_asc(task::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(|row| {
            let notes = notes.remove(&row.id).unwrap_or_default();
            let mut task = convert::task_from_row(row);
            task.notes = notes;
            task
        })
        .collect();
    let mut slots = [None; SLOT_COUNT];
    for row in slot::Entity::find().all(db).await? {
        if let Some(s) = usize::try_from(row.position)
            .ok()
            .and_then(|i| slots.get_mut(i))
        {
            *s = row.task_id.map(|id| id as u64);
        }
    }
    let next_id = meta::Entity::find_by_id(meta::ID)
        .one(db)
        .await?
        .map_or(0, |m| m.next_id as u64);
    Ok(Board {
        tasks,
        slots,
        next_id,
    })
}

async fn write(db: &DatabaseConnection, changes: &Changes<'_>) -> Result<(), DbErr> {
    let txn = db.begin().await?;
    for &(t, notes_changed) in &changes.upserts {
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
        if notes_changed {
            // Notes have no identity in the model, so replace them all.
            note::Entity::delete_many()
                .filter(note::Column::TaskId.eq(t.id as i64))
                .exec(&txn)
                .await?;
            if !t.notes.is_empty() {
                note::Entity::insert_many(convert::note_rows(t))
                    .exec_without_returning(&txn)
                    .await?;
            }
        }
    }
    if !changes.deletes.is_empty() {
        task::Entity::delete_many()
            .filter(task::Column::Id.is_in(changes.deletes.iter().map(|&id| id as i64)))
            .exec(&txn)
            .await?;
    }
    for &(i, task_id) in &changes.slots {
        slot::Entity::insert(slot::ActiveModel {
            position: Set(i as i32),
            task_id: Set(task_id.map(|id| id as i64)),
        })
        .on_conflict(
            OnConflict::column(slot::Column::Position)
                .update_column(slot::Column::TaskId)
                .to_owned(),
        )
        .exec_without_returning(&txn)
        .await?;
    }
    if let Some(next_id) = changes.next_id {
        meta::Entity::insert(meta::ActiveModel {
            id: Set(meta::ID),
            next_id: Set(next_id as i64),
        })
        .on_conflict(
            OnConflict::column(meta::Column::Id)
                .update_column(meta::Column::NextId)
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
