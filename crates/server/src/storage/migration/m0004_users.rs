//! Users, and an owner for every task, comment and focus slot.
//!
//! On SQLite this rebuilds the single-user tables with `owner_id`. Existing
//! rows go to the **local user** (`iss` and `sub` "local"), the one the
//! native app's local mode signs in as, so a board kept before users keeps
//! working there. On any other database this creates the whole schema.

use chrono::Utc;
use sea_orm::{DatabaseBackend, Statement};
use sea_orm_migration::{prelude::*, schema::*};
use uuid::Uuid;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() == DatabaseBackend::Sqlite {
            sqlite(manager).await
        } else {
            create(manager).await
        }
    }

    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }
}

/// Adds users to a SQLite database made by m0001–m0003.
async fn sqlite(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let db = manager.get_connection();
    db.execute_unprepared(
        r#"
CREATE TABLE "users" (
    "id" text NOT NULL PRIMARY KEY,
    "iss" text NOT NULL,
    "sub" text NOT NULL,
    "started" boolean NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL
);
CREATE UNIQUE INDEX "idx_users_identity" ON "users" ("iss", "sub");
"#,
    )
    .await?;
    let has_board = db
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            r#"SELECT EXISTS (SELECT 1 FROM "task")"#,
        ))
        .await?
        .map(|row| row.try_get_by_index::<bool>(0))
        .transpose()?
        .unwrap_or(false);
    // Without a board there's nothing to own yet; the local user is made
    // when local mode first signs in.
    let local = Uuid::now_v7().to_string();
    if has_board {
        db.execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            r#"INSERT INTO "users" VALUES (?, 'local', 'local', true, ?)"#,
            [local.clone().into(), Utc::now().into()],
        ))
        .await?;
    }
    // `local` is a UUID, so splicing it in is safe.
    db.execute_unprepared(&format!(
        r#"
CREATE TABLE "task_new" (
    "id" text NOT NULL PRIMARY KEY,
    "owner_id" text NOT NULL,
    "title" text NOT NULL,
    "done" boolean NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL,
    "created_at" timestamp_with_timezone_text NULL,
    "done_at" timestamp_with_timezone_text NULL,
    "parent_id" text NULL,
    "folded" boolean NOT NULL,
    FOREIGN KEY ("owner_id") REFERENCES "users" ("id") ON DELETE CASCADE,
    FOREIGN KEY ("parent_id") REFERENCES "task_new" ("id") ON DELETE SET NULL
);
INSERT INTO "task_new"
SELECT "id", '{local}', "title", "done", "updated_at", "created_at", "done_at",
    "parent_id", "folded"
FROM "task";

CREATE TABLE "note_new" (
    "id" text NOT NULL PRIMARY KEY,
    "owner_id" text NOT NULL,
    "task_id" text NOT NULL,
    "position" integer NOT NULL,
    "at" timestamp_with_timezone_text NOT NULL,
    "text" text NOT NULL,
    "starred" boolean NOT NULL,
    "blocked_at" timestamp_with_timezone_text NULL,
    "blocker_state" varchar NULL,
    "blocker_resolved_at" timestamp_with_timezone_text NULL,
    "next_step_state" varchar NULL,
    "next_step_since" timestamp_with_timezone_text NULL,
    "next_step_at" timestamp_with_timezone_text NULL,
    "next_step_subtask" text NULL,
    "action_kind" varchar NULL,
    "action_quote" text NULL,
    "action_blocked_since" timestamp_with_timezone_text NULL,
    FOREIGN KEY ("owner_id") REFERENCES "users" ("id") ON DELETE CASCADE,
    FOREIGN KEY ("task_id") REFERENCES "task_new" ("id") ON DELETE CASCADE
);
INSERT INTO "note_new"
SELECT "id", '{local}', "task_id", "position", "at", "text", "starred", "blocked_at",
    "blocker_state", "blocker_resolved_at", "next_step_state", "next_step_since",
    "next_step_at", "next_step_subtask", "action_kind", "action_quote",
    "action_blocked_since"
FROM "note";

CREATE TABLE "slot_new" (
    "owner_id" text NOT NULL,
    "position" integer NOT NULL,
    "task_id" text NULL,
    PRIMARY KEY ("owner_id", "position"),
    FOREIGN KEY ("owner_id") REFERENCES "users" ("id") ON DELETE CASCADE,
    FOREIGN KEY ("task_id") REFERENCES "task_new" ("id") ON DELETE SET NULL
);
-- An empty slot needs no row.
INSERT INTO "slot_new"
SELECT '{local}', "position", "task_id" FROM "slot" WHERE "task_id" IS NOT NULL;

DROP TABLE "note";
DROP TABLE "slot";
DROP TABLE "task";
ALTER TABLE "task_new" RENAME TO "task";
ALTER TABLE "note_new" RENAME TO "note";
ALTER TABLE "slot_new" RENAME TO "slot";
CREATE INDEX "idx_task_owner" ON "task" ("owner_id");
CREATE INDEX "idx_note_task" ON "note" ("task_id", "position");
"#
    ))
    .await?;
    Ok(())
}

/// The whole schema, for a new database.
async fn create(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(Users::Table)
                .col(text(Users::Id).primary_key())
                .col(text(Users::Iss))
                .col(text(Users::Sub))
                .col(boolean(Users::Started))
                .col(timestamp_with_time_zone(Users::CreatedAt))
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_users_identity")
                .table(Users::Table)
                .col(Users::Iss)
                .col(Users::Sub)
                .unique()
                .to_owned(),
        )
        .await?;
    manager
        .create_table(
            Table::create()
                .table(Task::Table)
                .col(text(Task::Id).primary_key())
                .col(text(Task::OwnerId))
                .col(text(Task::Title))
                .col(boolean(Task::Done))
                .col(timestamp_with_time_zone(Task::UpdatedAt))
                .col(timestamp_with_time_zone_null(Task::CreatedAt))
                .col(timestamp_with_time_zone_null(Task::DoneAt))
                .col(text_null(Task::ParentId))
                .col(boolean(Task::Folded))
                .foreign_key(&mut owner(Task::Table, Task::OwnerId))
                .foreign_key(
                    ForeignKey::create()
                        .from(Task::Table, Task::ParentId)
                        .to(Task::Table, Task::Id)
                        .on_delete(ForeignKeyAction::SetNull),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_task_owner")
                .table(Task::Table)
                .col(Task::OwnerId)
                .to_owned(),
        )
        .await?;
    manager
        .create_table(
            Table::create()
                .table(Note::Table)
                .col(text(Note::Id).primary_key())
                .col(text(Note::OwnerId))
                .col(text(Note::TaskId))
                .col(integer(Note::Position))
                .col(timestamp_with_time_zone(Note::At))
                .col(text(Note::Text))
                .col(boolean(Note::Starred))
                .col(timestamp_with_time_zone_null(Note::BlockedAt))
                .col(string_null(Note::BlockerState))
                .col(timestamp_with_time_zone_null(Note::BlockerResolvedAt))
                .col(string_null(Note::NextStepState))
                .col(timestamp_with_time_zone_null(Note::NextStepSince))
                .col(timestamp_with_time_zone_null(Note::NextStepAt))
                .col(text_null(Note::NextStepSubtask))
                .col(string_null(Note::ActionKind))
                .col(text_null(Note::ActionQuote))
                .col(timestamp_with_time_zone_null(Note::ActionBlockedSince))
                .foreign_key(&mut owner(Note::Table, Note::OwnerId))
                .foreign_key(
                    ForeignKey::create()
                        .from(Note::Table, Note::TaskId)
                        .to(Task::Table, Task::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_note_task")
                .table(Note::Table)
                .col(Note::TaskId)
                .col(Note::Position)
                .to_owned(),
        )
        .await?;
    manager
        .create_table(
            Table::create()
                .table(Slot::Table)
                .col(text(Slot::OwnerId))
                .col(integer(Slot::Position))
                .col(text_null(Slot::TaskId))
                .primary_key(Index::create().col(Slot::OwnerId).col(Slot::Position))
                .foreign_key(&mut owner(Slot::Table, Slot::OwnerId))
                .foreign_key(
                    ForeignKey::create()
                        .from(Slot::Table, Slot::TaskId)
                        .to(Task::Table, Task::Id)
                        .on_delete(ForeignKeyAction::SetNull),
                )
                .to_owned(),
        )
        .await
}

/// A row's owner: deleting a user deletes what they own.
fn owner<T: IntoIden + 'static, C: IntoIden + 'static>(
    table: T,
    column: C,
) -> ForeignKeyCreateStatement {
    ForeignKey::create()
        .from(table, column)
        .to(Users::Table, Users::Id)
        .on_delete(ForeignKeyAction::Cascade)
        .to_owned()
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
    Iss,
    Sub,
    Started,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Task {
    Table,
    Id,
    OwnerId,
    Title,
    Done,
    UpdatedAt,
    CreatedAt,
    DoneAt,
    ParentId,
    Folded,
}

#[derive(DeriveIden)]
enum Note {
    Table,
    Id,
    OwnerId,
    TaskId,
    Position,
    At,
    Text,
    Starred,
    BlockedAt,
    BlockerState,
    BlockerResolvedAt,
    NextStepState,
    NextStepSince,
    NextStepAt,
    NextStepSubtask,
    ActionKind,
    ActionQuote,
    ActionBlockedSince,
}

#[derive(DeriveIden)]
enum Slot {
    Table,
    OwnerId,
    Position,
    TaskId,
}
