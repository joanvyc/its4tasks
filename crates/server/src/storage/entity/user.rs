use sea_orm::entity::prelude::*;

/// Someone who signed in: `sub` as named by the authority `iss`.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "users")]
pub struct Model {
    /// A UUID v7 as text.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    #[sea_orm(column_type = "Text")]
    pub iss: String,
    #[sea_orm(column_type = "Text")]
    pub sub: String,
    /// Chose to start a new board or import one. New users haven't.
    pub started: bool,
    pub created_at: DateTimeUtc,
    /// `its4tasks_core::api::Settings` as JSON.
    #[sea_orm(column_type = "Text")]
    pub settings: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
