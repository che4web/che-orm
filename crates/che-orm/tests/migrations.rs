#![allow(dead_code)]

use std::time::{SystemTime, UNIX_EPOCH};

use che_orm::{
    FieldSchema, FieldType, Model, ModelSchema, Schema, SchemaChange, diff_schemas,
    sqlite_migration_sql,
};

#[derive(Debug, Clone, Model)]
#[model(table = "users")]
struct User {
    #[field(primary_key)]
    id: i64,
    email: String,
    #[field(default = true)]
    is_active: bool,
}

#[test]
fn diff_empty_schema_creates_table() {
    let old = Schema::empty();
    let new = Schema::from_model::<User>();
    let migration = diff_schemas(&old, &new);

    assert!(matches!(
        migration.changes.as_slice(),
        [SchemaChange::CreateTable(model)] if model.table == "users"
    ));
}

#[test]
fn diff_added_field_generates_add_column() {
    let old = Schema::from_models(vec![ModelSchema {
        table: "users".to_string(),
        fields: vec![id_field()],
    }]);
    let new = Schema::from_models(vec![ModelSchema {
        table: "users".to_string(),
        fields: vec![id_field(), email_field()],
    }]);

    let migration = diff_schemas(&old, &new);

    assert!(matches!(
        migration.changes.as_slice(),
        [SchemaChange::AddColumn { table, field }] if table == "users" && field.name == "email"
    ));

    let sql = sqlite_migration_sql(&migration);
    assert_eq!(sql, "ALTER TABLE users ADD COLUMN email TEXT NOT NULL;");
}

#[test]
fn diff_removed_field_marks_drop_column_as_manual_sql() {
    let old = Schema::from_models(vec![ModelSchema {
        table: "users".to_string(),
        fields: vec![id_field(), email_field()],
    }]);
    let new = Schema::from_models(vec![ModelSchema {
        table: "users".to_string(),
        fields: vec![id_field()],
    }]);

    let migration = diff_schemas(&old, &new);
    let sql = sqlite_migration_sql(&migration);

    assert!(matches!(
        migration.changes.as_slice(),
        [SchemaChange::DropColumn { table, column }] if table == "users" && column == "email"
    ));
    assert!(sql.contains("does not drop columns automatically"));
}

#[test]
fn schema_snapshot_roundtrip_json() {
    let schema = Schema::from_model::<User>();
    let path = std::env::temp_dir().join(format!(
        "che_orm_schema_{}.json",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));

    schema.save(&path).unwrap();
    let loaded = Schema::load(&path).unwrap();
    std::fs::remove_file(path).unwrap();

    assert_eq!(loaded, schema);
}

#[test]
fn migration_sql_for_create_table() {
    let migration = diff_schemas(&Schema::empty(), &Schema::from_model::<User>());
    let sql = sqlite_migration_sql(&migration);

    assert!(sql.contains("CREATE TABLE IF NOT EXISTS users"));
    assert!(sql.contains("id INTEGER PRIMARY KEY AUTOINCREMENT"));
    assert!(sql.contains("email TEXT NOT NULL"));
    assert!(sql.contains("is_active BOOLEAN NOT NULL DEFAULT true"));
}

fn id_field() -> FieldSchema {
    FieldSchema {
        name: "id".to_string(),
        ty: FieldType::Integer,
        primary_key: true,
        nullable: false,
        auto: true,
        unique: false,
        max_length: None,
        default: None,
        foreign_key: None,
    }
}

fn email_field() -> FieldSchema {
    FieldSchema {
        name: "email".to_string(),
        ty: FieldType::Text,
        primary_key: false,
        nullable: false,
        auto: false,
        unique: false,
        max_length: None,
        default: None,
        foreign_key: None,
    }
}
