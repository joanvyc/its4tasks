use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "task")]
pub struct Model {
    /// Assigned by the model (`Board::next_id`), not by the database.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i64,
    #[sea_orm(column_type = "Text")]
    pub title: String,
    pub done: bool,
    pub updated_at: DateTimeUtc,
    pub created_at: Option<DateTimeUtc>,
    pub done_at: Option<DateTimeUtc>,
    pub parent_id: Option<i64>,
    pub folded: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
