//! Comments get the model's UUID as their id instead of a row number.
//!
//! SQLite can't change a primary key, so the table is rebuilt. Existing
//! comments get random v4 UUIDs, as text like `Uuid::to_string`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
CREATE TABLE "note_new" (
    "id" text NOT NULL PRIMARY KEY,
    "task_id" integer NOT NULL,
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
    "next_step_subtask" integer NULL,
    "action_kind" varchar NULL,
    "action_quote" text NULL,
    "action_blocked_since" timestamp_with_timezone_text NULL,
    FOREIGN KEY ("task_id") REFERENCES "task" ("id") ON DELETE CASCADE
);
INSERT INTO "note_new"
SELECT
    lower(
        hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' ||
        substr(hex(randomblob(2)), 2) || '-' ||
        substr('89ab', 1 + (abs(random()) % 4), 1) ||
        substr(hex(randomblob(2)), 2) || '-' || hex(randomblob(6))
    ),
    "task_id", "position", "at", "text", "starred", "blocked_at",
    "blocker_state", "blocker_resolved_at", "next_step_state",
    "next_step_since", "next_step_at", "next_step_subtask", "action_kind",
    "action_quote", "action_blocked_since"
FROM "note";
DROP TABLE "note";
ALTER TABLE "note_new" RENAME TO "note";
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
