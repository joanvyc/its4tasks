use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Task::Table)
                    .col(big_integer(Task::Id).primary_key())
                    .col(text(Task::Title))
                    .col(boolean(Task::Done))
                    .col(timestamp_with_time_zone(Task::UpdatedAt))
                    .col(timestamp_with_time_zone_null(Task::CreatedAt))
                    .col(timestamp_with_time_zone_null(Task::DoneAt))
                    .col(big_integer_null(Task::ParentId))
                    .col(boolean(Task::Folded))
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
            .create_table(
                Table::create()
                    .table(Note::Table)
                    .col(pk_auto(Note::Id))
                    .col(big_integer(Note::TaskId))
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
                    .col(big_integer_null(Note::NextStepSubtask))
                    .col(string_null(Note::ActionKind))
                    .col(text_null(Note::ActionQuote))
                    .col(timestamp_with_time_zone_null(Note::ActionBlockedSince))
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
                    .col(integer(Slot::Position).primary_key())
                    .col(big_integer_null(Slot::TaskId))
                    .foreign_key(
                        ForeignKey::create()
                            .from(Slot::Table, Slot::TaskId)
                            .to(Task::Table, Task::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(Meta::Table)
                    .col(integer(Meta::Id).primary_key())
                    .col(big_integer(Meta::NextId))
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Task {
    Table,
    Id,
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
    Position,
    TaskId,
}

#[derive(DeriveIden)]
enum Meta {
    Table,
    Id,
    NextId,
}
