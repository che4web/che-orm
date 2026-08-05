#![allow(dead_code)]

use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

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

#[derive(Debug, Clone, Model)]
#[model(table = "migration_users")]
struct OldUser {
    #[field(primary_key)]
    id: i64,
    email: String,
    name: String,
}

#[derive(Debug, Clone, Model)]
#[model(table = "migration_users")]
struct CurrentUser {
    #[field(primary_key)]
    id: i64,
    name: String,
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
fn diff_removed_field_generates_table_rebuild_sql() {
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
    assert!(sql.contains("CREATE TABLE \"__che_orm_new_users\""));
    assert!(
        sql.contains("INSERT INTO \"__che_orm_new_users\" (\"id\") SELECT \"id\" FROM \"users\";")
    );
    assert!(sql.contains("DROP TABLE \"users\";"));
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

#[tokio::test]
async fn migration_rebuilds_table_when_column_is_removed() {
    let db = che_orm::SqliteBackend::connect("sqlite::memory:")
        .await
        .unwrap();
    db.create_table::<OldUser>().await.unwrap();
    let old_user = OldUser::objects(&db)
        .create()
        .set("email", "removed@example.com")
        .set("name", "Alice")
        .execute()
        .await
        .unwrap();

    let migration = diff_schemas(
        &Schema::from_model::<OldUser>(),
        &Schema::from_model::<CurrentUser>(),
    );
    let migrations_dir = std::env::temp_dir().join(format!(
        "che_orm_column_drop_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&migrations_dir).unwrap();
    fs::write(
        migrations_dir.join("0001_remove_email.sql"),
        sqlite_migration_sql(&migration),
    )
    .unwrap();

    db.apply_migrations_dir(&migrations_dir).await.unwrap();

    let current_user = CurrentUser::objects(&db).get(old_user.id).await.unwrap();
    assert_eq!(current_user.name, "Alice");
    fs::remove_dir_all(migrations_dir).unwrap();
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
        auto_now_add: false,
        auto_now: false,
        foreign_key: None,
        choices: None,
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
        auto_now_add: false,
        auto_now: false,
        foreign_key: None,
        choices: None,
    }
}
