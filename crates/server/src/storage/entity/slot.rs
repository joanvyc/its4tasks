use sea_orm::entity::prelude::*;

/// One of a user's focus cards. A missing row means an empty slot.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "slot")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub owner_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub position: i32,
    pub task_id: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
