use che_orm::{Model, time::OffsetDateTime};

#[derive(Debug, Model)]
#[orm(table = "tasks", index("title"))]
pub struct Task {
    #[orm(primary_key)]
    pub id: i64,
    #[orm(check = "length(title) > 0")]
    pub title: String,
    #[orm(default = "false")]
    pub done: bool,
    #[orm(auto_now_add)]
    pub created_at: OffsetDateTime,
    #[orm(auto_now)]
    pub updated_at: OffsetDateTime,
}
