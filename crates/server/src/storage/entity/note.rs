use sea_orm::entity::prelude::*;

/// A comment on a task. The model's `Blocker`, `NextStep` and `NoteAction`
/// enums are flattened into a state column plus the fields they carry.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "note")]
pub struct Model {
    /// The model's `Note::id`, a UUID as text.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// The user whose board it's on, as its task's.
    pub owner_id: String,
    pub task_id: String,
    /// Order within the task.
    pub position: i32,
    pub at: DateTimeUtc,
    #[sea_orm(column_type = "Text")]
    pub text: String,
    pub starred: bool,
    pub blocked_at: Option<DateTimeUtc>,
    /// `open` or `resolved`.
    pub blocker_state: Option<String>,
    pub blocker_resolved_at: Option<DateTimeUtc>,
    /// `open`, `done` or `promoted`.
    pub next_step_state: Option<String>,
    pub next_step_since: Option<DateTimeUtc>,
    pub next_step_at: Option<DateTimeUtc>,
    pub next_step_subtask: Option<String>,
    /// `unblocked` or `closed`.
    pub action_kind: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub action_quote: Option<String>,
    pub action_blocked_since: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
