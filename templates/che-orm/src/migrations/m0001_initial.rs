//! Frozen historical schema. Add a new migration when changing the model.
//! Do not derive an applied migration from Task::schema().
use che_orm::{
    ColumnSchema, ColumnType, Migration, MigrationId, MigrationOperation, ModelKey, ModelState,
    TableSchema, TableState,
};

pub fn migration() -> Migration {
    let mut id = ColumnSchema::new("id", ColumnType::Integer, false);
    id.primary_key = true;
    let mut title = ColumnSchema::new("title", ColumnType::Text, false);
    title.check = Some("length(title) > 0");
    let mut done = ColumnSchema::new("done", ColumnType::Boolean, false);
    done.default = Some("false");
    let mut created_at = ColumnSchema::new("created_at", ColumnType::DateTime, false);
    created_at.auto_now_add = true;
    let mut updated_at = ColumnSchema::new("updated_at", ColumnType::DateTime, false);
    updated_at.auto_now = true;

    let table = TableSchema {
        name: "tasks",
        columns: vec![id, title, done, created_at, updated_at],
        unique_constraints: vec![],
        indexes: vec![vec!["title"]],
    };
    Migration::new(
        MigrationId {
            app: "tasks".into(),
            name: "0001_initial".into(),
        },
        vec![],
        vec![MigrationOperation::CreateModel {
            model: ModelState {
                key: ModelKey {
                    app: "tasks".into(),
                    name: "Task".into(),
                },
                table: TableState::from(&table),
            },
        }],
    )
}
