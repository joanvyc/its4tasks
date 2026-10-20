//! Tasks get the model's UUID (v7, time-ordered) as their id instead of a
//! number handed out by `next_id`, which goes away with the `meta` table.
//!
//! Existing ids are mapped in ascending order to freshly generated v7 UUIDs,
//! which keeps the task list in the same order. Comments' `task_id` and
//! promoted steps' `next_step_subtask`, and the slots, follow the map.
//! SQLite can't change a primary key, so the tables are rebuilt.

use sea_orm::Statement;
use sea_orm_migration::prelude::*;
use uuid::Uuid;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Reshapes data from the app's SQLite days; other databases start
        // with m0004's schema.
        if manager.get_database_backend() != sea_orm::DatabaseBackend::Sqlite {
            return Ok(());
        }
        let db = manager.get_connection();
        // Includes subtask ids of promoted steps whose subtask was deleted,
        // so they still map to something.
        let rows = db
            .query_all_raw(Statement::from_string(
                db.get_database_backend(),
                r#"SELECT id FROM task
                   UNION SELECT next_step_subtask FROM note WHERE next_step_subtask IS NOT NULL
                   ORDER BY 1"#,
            ))
            .await?;
        let mut map = String::from(
            r#"CREATE TEMP TABLE "id_map" ("old" integer PRIMARY KEY, "new" text NOT NULL);"#,
        );
        for row in rows {
            let old: i64 = row.try_get_by_index(0)?;
            map.push_str(&format!(
                r#"INSERT INTO "id_map" VALUES ({old}, '{}');"#,
                Uuid::now_v7()
            ));
        }
        db.execute_unprepared(&map).await?;
        db.execute_unprepared(
            r#"
CREATE TABLE "task_new" (
    "id" text NOT NULL PRIMARY KEY,
    "title" text NOT NULL,
    "done" boolean NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL,
    "created_at" timestamp_with_timezone_text NULL,
    "done_at" timestamp_with_timezone_text NULL,
    "parent_id" text NULL,
    "folded" boolean NOT NULL,
    FOREIGN KEY ("parent_id") REFERENCES "task_new" ("id") ON DELETE SET NULL
);
INSERT INTO "task_new"
SELECT m."new", t."title", t."done", t."updated_at", t."created_at", t."done_at", p."new", t."folded"
FROM "task" t
JOIN "id_map" m ON m."old" = t."id"
LEFT JOIN "id_map" p ON p."old" = t."parent_id";

CREATE TABLE "note_new" (
    "id" text NOT NULL PRIMARY KEY,
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
    FOREIGN KEY ("task_id") REFERENCES "task_new" ("id") ON DELETE CASCADE
);
INSERT INTO "note_new"
SELECT n."id", m."new", n."position", n."at", n."text", n."starred", n."blocked_at",
    n."blocker_state", n."blocker_resolved_at", n."next_step_state",
    n."next_step_since", n."next_step_at", s."new", n."action_kind",
    n."action_quote", n."action_blocked_since"
FROM "note" n
JOIN "id_map" m ON m."old" = n."task_id"
LEFT JOIN "id_map" s ON s."old" = n."next_step_subtask";

CREATE TABLE "slot_new" (
    "position" integer NOT NULL PRIMARY KEY,
    "task_id" text NULL,
    FOREIGN KEY ("task_id") REFERENCES "task_new" ("id") ON DELETE SET NULL
);
INSERT INTO "slot_new"
SELECT s."position", m."new"
FROM "slot" s
LEFT JOIN "id_map" m ON m."old" = s."task_id";

DROP TABLE "note";
DROP TABLE "slot";
DROP TABLE "task";
DROP TABLE "meta";
DROP TABLE "id_map";
-- Renaming also points the foreign keys above at "task".
ALTER TABLE "task_new" RENAME TO "task";
ALTER TABLE "note_new" RENAME TO "note";
ALTER TABLE "slot_new" RENAME TO "slot";
CREATE INDEX "idx_note_task" ON "note" ("task_id", "position");
"#,
        )
        .await?;
        Ok(())
    }

    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }
}
