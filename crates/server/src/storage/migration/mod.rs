//! The schema's history. m0001–m0003 reshaped the single-user SQLite board
//! and do nothing elsewhere; m0004 brings users and ownership, and on a new
//! Postgres database creates the whole schema as it was then; m0005 adds
//! users' settings.

use sea_orm_migration::prelude::*;

mod m0001_create_board;
mod m0002_note_uuid;
mod m0003_task_uuid;
mod m0004_users;
mod m0005_user_settings;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m0001_create_board::Migration),
            Box::new(m0002_note_uuid::Migration),
            Box::new(m0003_task_uuid::Migration),
            Box::new(m0004_users::Migration),
            Box::new(m0005_user_settings::Migration),
        ]
    }
}
