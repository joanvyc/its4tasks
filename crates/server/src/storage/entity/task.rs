use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "task")]
pub struct Model {
    /// The model's `Task::id`, a UUID as text.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// The user whose board it's on.
    pub owner_id: String,
    #[sea_orm(column_type = "Text")]
    pub title: String,
    pub done: bool,
    pub updated_at: DateTimeUtc,
    pub created_at: Option<DateTimeUtc>,
    pub done_at: Option<DateTimeUtc>,
    pub parent_id: Option<String>,
    pub folded: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
