use sea_orm_migration::prelude::*;

mod m0001_create_board;
mod m0002_note_uuid;
mod m0003_task_uuid;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m0001_create_board::Migration),
            Box::new(m0002_note_uuid::Migration),
            Box::new(m0003_task_uuid::Migration),
        ]
    }
}
