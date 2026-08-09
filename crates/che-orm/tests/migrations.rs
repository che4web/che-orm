#![allow(dead_code)]

use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use che_orm::{
    FieldSchema, FieldType, ForeignKeyAction, ForeignKeySchema, Model, ModelSchema, Schema,
    SchemaChange, SqliteBackend, diff_schemas, sqlite_migration_sql, validate_migration,
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

#[derive(Debug, Clone, Model)]
#[model(table = "indexed_users")]
struct IndexedUser {
    #[field(primary_key)]
    id: i64,
    #[field(index)]
    email: String,
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
        indexes: Vec::new(),
    }]);
    let new = Schema::from_models(vec![ModelSchema {
        table: "users".to_string(),
        fields: vec![id_field(), email_field()],
        indexes: Vec::new(),
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
        indexes: Vec::new(),
    }]);
    let new = Schema::from_models(vec![ModelSchema {
        table: "users".to_string(),
        fields: vec![id_field()],
        indexes: Vec::new(),
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
fn diff_changed_field_generates_alter_column() {
    let mut old_email = email_field();
    old_email.nullable = true;
    let mut new_email = email_field();
    new_email.unique = true;
    new_email.max_length = Some(255);

    let migration = diff_schemas(
        &Schema::from_models(vec![ModelSchema {
            table: "users".to_string(),
            fields: vec![id_field(), old_email],
            indexes: Vec::new(),
        }]),
        &Schema::from_models(vec![ModelSchema {
            table: "users".to_string(),
            fields: vec![id_field(), new_email],
            indexes: Vec::new(),
        }]),
    );

    assert!(matches!(
        migration.changes.as_slice(),
        [SchemaChange::AlterColumn { table, old, new }]
            if table == "users" && old.nullable && new.unique
    ));

    let sql = sqlite_migration_sql(&migration);
    assert!(sql.contains("CREATE TABLE \"__che_orm_new_users\""));
    assert!(sql.contains("email TEXT NOT NULL UNIQUE"));
    assert!(sql.contains("CHECK (length(email) <= 255)"));
}

#[test]
fn unsafe_required_column_requires_default() {
    let migration = diff_schemas(
        &Schema::from_model::<User>(),
        &Schema::from_models(vec![ModelSchema {
            table: "users".to_string(),
            fields: vec![id_field(), email_field(), is_admin_field()],
            indexes: Vec::new(),
        }]),
    );

    let error = validate_migration(&migration).unwrap_err();
    assert!(error.to_string().contains("users.is_admin"));
}

#[test]
fn foreign_key_validation_rejects_invalid_source_and_action() {
    let mut field = email_field();
    field.foreign_key = Some(ForeignKeySchema {
        table: "users".to_string(),
        on_delete: ForeignKeyAction::SetNull,
    });
    let migration = diff_schemas(
        &Schema::empty(),
        &Schema::from_models(vec![ModelSchema {
            table: "users".to_string(),
            fields: vec![id_field(), field],
            indexes: Vec::new(),
        }]),
    );
    assert!(validate_migration(&migration).is_err());
}

#[test]
fn migration_creates_fk_parent_before_child() {
    let parent = ModelSchema {
        table: "parents".to_string(),
        fields: vec![id_field()],
        indexes: Vec::new(),
    };
    let mut child_id = id_field();
    child_id.name = "id".to_string();
    let mut parent_id = email_field();
    parent_id.name = "parent_id".to_string();
    parent_id.ty = FieldType::Integer;
    parent_id.foreign_key = Some(ForeignKeySchema {
        table: "parents".to_string(),
        on_delete: ForeignKeyAction::NoAction,
    });
    let migration = diff_schemas(
        &Schema::empty(),
        &Schema::from_models(vec![
            ModelSchema {
                table: "children".to_string(),
                fields: vec![child_id, parent_id],
                indexes: Vec::new(),
            },
            parent,
        ]),
    );
    let sql = sqlite_migration_sql(&migration);
    assert!(
        sql.find("CREATE TABLE IF NOT EXISTS parents").unwrap()
            < sql.find("CREATE TABLE IF NOT EXISTS children").unwrap()
    );
}

#[test]
fn required_fk_with_default_uses_table_rebuild() {
    let mut parent_id = email_field();
    parent_id.name = "parent_id".to_string();
    parent_id.ty = FieldType::Integer;
    parent_id.default = Some("1".to_string());
    parent_id.foreign_key = Some(ForeignKeySchema {
        table: "parents".to_string(),
        on_delete: ForeignKeyAction::NoAction,
    });
    let migration = diff_schemas(
        &Schema::from_models(vec![ModelSchema {
            table: "children".to_string(),
            fields: vec![id_field()],
            indexes: Vec::new(),
        }]),
        &Schema::from_models(vec![ModelSchema {
            table: "children".to_string(),
            fields: vec![id_field(), parent_id],
            indexes: Vec::new(),
        }]),
    );
    let sql = sqlite_migration_sql(&migration);
    assert!(
        sql.contains("CREATE TABLE \"__che_orm_new_children\""),
        "{sql}"
    );
    assert!(!sql.contains("ALTER TABLE children ADD COLUMN"));
}

#[test]
fn nullable_fk_with_default_uses_table_rebuild() {
    let mut parent_id = email_field();
    parent_id.name = "parent_id".to_string();
    parent_id.ty = FieldType::Integer;
    parent_id.nullable = true;
    parent_id.default = Some("1".to_string());
    parent_id.foreign_key = Some(ForeignKeySchema {
        table: "parents".to_string(),
        on_delete: ForeignKeyAction::NoAction,
    });
    let migration = diff_schemas(
        &Schema::from_models(vec![ModelSchema {
            table: "children".to_string(),
            fields: vec![id_field()],
            indexes: Vec::new(),
        }]),
        &Schema::from_models(vec![ModelSchema {
            table: "children".to_string(),
            fields: vec![id_field(), parent_id],
            indexes: Vec::new(),
        }]),
    );
    let sql = sqlite_migration_sql(&migration);
    assert!(sql.contains("CREATE TABLE \"__che_orm_new_children\""));
    assert!(!sql.contains("ALTER TABLE children ADD COLUMN"));
}

#[tokio::test]
async fn migration_rebuilds_when_adding_fk_with_default() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.apply_sql(
        "CREATE TABLE parents (id INTEGER PRIMARY KEY AUTOINCREMENT);\
         INSERT INTO parents DEFAULT VALUES;\
         CREATE TABLE children (id INTEGER PRIMARY KEY AUTOINCREMENT);\
         INSERT INTO children DEFAULT VALUES;",
    )
    .await
    .unwrap();

    let parent = ModelSchema {
        table: "parents".to_string(),
        fields: vec![id_field()],
        indexes: Vec::new(),
    };
    let mut parent_id = email_field();
    parent_id.name = "parent_id".to_string();
    parent_id.ty = FieldType::Integer;
    parent_id.default = Some("1".to_string());
    parent_id.foreign_key = Some(ForeignKeySchema {
        table: "parents".to_string(),
        on_delete: ForeignKeyAction::NoAction,
    });
    let old = Schema::from_models(vec![
        parent.clone(),
        ModelSchema {
            table: "children".to_string(),
            fields: vec![id_field()],
            indexes: Vec::new(),
        },
    ]);
    let new = Schema::from_models(vec![
        parent,
        ModelSchema {
            table: "children".to_string(),
            fields: vec![id_field(), parent_id],
            indexes: Vec::new(),
        },
    ]);
    let migration = diff_schemas(&old, &new);
    validate_migration(&migration).unwrap();
    db.apply_sql(&sqlite_migration_sql(&migration))
        .await
        .unwrap();

    let parent_id: i64 = sqlx::query_scalar("SELECT parent_id FROM children WHERE id = 1")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(parent_id, 1);
}

#[test]
fn migration_drops_child_before_parent() {
    let parent = ModelSchema {
        table: "parents".to_string(),
        fields: vec![id_field()],
        indexes: Vec::new(),
    };
    let mut parent_id = email_field();
    parent_id.name = "parent_id".to_string();
    parent_id.ty = FieldType::Integer;
    parent_id.foreign_key = Some(ForeignKeySchema {
        table: "parents".to_string(),
        on_delete: ForeignKeyAction::NoAction,
    });
    let old = Schema::from_models(vec![
        parent,
        ModelSchema {
            table: "children".to_string(),
            fields: vec![id_field(), parent_id],
            indexes: Vec::new(),
        },
    ]);
    let sql = sqlite_migration_sql(&diff_schemas(&old, &Schema::empty()));
    assert!(
        sql.find("DROP TABLE IF EXISTS children").unwrap()
            < sql.find("DROP TABLE IF EXISTS parents").unwrap()
    );
}

#[tokio::test]
async fn parent_rebuild_preserves_cascade_children() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.apply_sql(
        "CREATE TABLE parents (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL);\
         CREATE TABLE children (id INTEGER PRIMARY KEY AUTOINCREMENT, parent_id INTEGER NOT NULL REFERENCES parents(id) ON DELETE CASCADE);\
         INSERT INTO parents (name) VALUES ('Parent');\
         INSERT INTO children (parent_id) VALUES (1);",
    )
    .await
    .unwrap();

    let mut parent_name = email_field();
    parent_name.name = "name".to_string();
    let parent = ModelSchema {
        table: "parents".to_string(),
        fields: vec![id_field(), parent_name.clone()],
        indexes: Vec::new(),
    };
    let mut changed_parent_name = parent_name;
    changed_parent_name.nullable = true;
    let mut parent_id = email_field();
    parent_id.name = "parent_id".to_string();
    parent_id.ty = FieldType::Integer;
    parent_id.foreign_key = Some(ForeignKeySchema {
        table: "parents".to_string(),
        on_delete: ForeignKeyAction::Cascade,
    });
    let child = ModelSchema {
        table: "children".to_string(),
        fields: vec![id_field(), parent_id.clone()],
        indexes: Vec::new(),
    };
    let old = Schema::from_models(vec![parent, child.clone()]);
    let new = Schema::from_models(vec![
        ModelSchema {
            table: "parents".to_string(),
            fields: vec![id_field(), changed_parent_name],
            indexes: Vec::new(),
        },
        child,
    ]);
    let migration = diff_schemas(&old, &new);
    let sql = sqlite_migration_sql(&migration);
    assert!(sql.starts_with("-- che-orm: sqlite-fk-rebuild"));
    db.apply_sql(&sql).await.unwrap();

    let child_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM children")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let parent_id: i64 = sqlx::query_scalar("SELECT parent_id FROM children")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(child_count, 1);
    assert_eq!(parent_id, 1);
}

#[tokio::test]
async fn rebuilding_parent_and_child_preserves_fk_rows() {
    for (action, sql_action) in [
        (ForeignKeyAction::Cascade, "CASCADE"),
        (ForeignKeyAction::Restrict, "RESTRICT"),
    ] {
        let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
        db.apply_sql(&format!(
            "CREATE TABLE parents (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL);\
             CREATE TABLE children (id INTEGER PRIMARY KEY AUTOINCREMENT, parent_id INTEGER NOT NULL REFERENCES parents(id) ON DELETE {sql_action}, name TEXT NOT NULL);\
             INSERT INTO parents (name) VALUES ('Parent');\
             INSERT INTO children (parent_id, name) VALUES (1, 'Child');"
        ))
        .await
        .unwrap();

        let mut parent_name = email_field();
        parent_name.name = "name".to_string();
        let mut child_name = parent_name.clone();
        let mut parent_id = email_field();
        parent_id.name = "parent_id".to_string();
        parent_id.ty = FieldType::Integer;
        parent_id.foreign_key = Some(ForeignKeySchema {
            table: "parents".to_string(),
            on_delete: action,
        });
        let old = Schema::from_models(vec![
            ModelSchema {
                table: "parents".to_string(),
                fields: vec![id_field(), parent_name.clone()],
                indexes: Vec::new(),
            },
            ModelSchema {
                table: "children".to_string(),
                fields: vec![id_field(), parent_id.clone(), child_name.clone()],
                indexes: Vec::new(),
            },
        ]);
        parent_name.nullable = true;
        child_name.nullable = true;
        let new = Schema::from_models(vec![
            ModelSchema {
                table: "parents".to_string(),
                fields: vec![id_field(), parent_name],
                indexes: Vec::new(),
            },
            ModelSchema {
                table: "children".to_string(),
                fields: vec![id_field(), parent_id, child_name],
                indexes: Vec::new(),
            },
        ]);

        let sql = sqlite_migration_sql(&diff_schemas(&old, &new));
        assert!(sql.starts_with("-- che-orm: sqlite-fk-rebuild"), "{sql}");
        db.apply_sql(&sql).await.unwrap();

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM children")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(rows, 1, "{sql_action}");
    }
}

#[tokio::test]
async fn foreign_keys_are_enabled_on_every_pool_connection() {
    let path = temporary_path("pool_foreign_keys");
    fs::File::create(&path).unwrap();
    let db = SqliteBackend::connect(&sqlite_url(&path)).await.unwrap();
    db.apply_sql(
        "CREATE TABLE parents (id INTEGER PRIMARY KEY);\
         CREATE TABLE children (parent_id INTEGER NOT NULL REFERENCES parents(id));",
    )
    .await
    .unwrap();

    let mut first = db.pool().acquire().await.unwrap();
    let mut second = db.pool().acquire().await.unwrap();
    for connection in [&mut first, &mut second] {
        let enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&mut **connection)
            .await
            .unwrap();
        assert_eq!(enabled, 1);
    }
    assert!(
        sqlx::query("INSERT INTO children (parent_id) VALUES (1)")
            .execute(&mut *second)
            .await
            .is_err()
    );

    drop(first);
    drop(second);
    fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn concurrent_safe_migration_is_applied_once() {
    let path = temporary_path("concurrent_migration");
    fs::File::create(&path).unwrap();
    let migrations_dir = std::env::temp_dir().join(format!(
        "che_orm_concurrent_migration_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&migrations_dir).unwrap();
    fs::write(
        migrations_dir.join("0001_safe.sql"),
        "-- che-orm: sqlite-fk-rebuild\nCREATE TABLE migrated (id INTEGER PRIMARY KEY);",
    )
    .unwrap();

    let url = sqlite_url(&path);
    let first = SqliteBackend::connect(&url).await.unwrap();
    let second = SqliteBackend::connect(&url).await.unwrap();
    let (first_result, second_result) = tokio::join!(
        first.apply_migrations_dir(&migrations_dir),
        second.apply_migrations_dir(&migrations_dir),
    );
    let first_applied = first_result.unwrap();
    let second_applied = second_result.unwrap();
    assert_eq!(first_applied.len() + second_applied.len(), 1);

    let migrations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _che_orm_migrations")
        .fetch_one(first.pool())
        .await
        .unwrap();
    assert_eq!(migrations, 1);

    fs::remove_dir_all(migrations_dir).unwrap();
    fs::remove_file(path).unwrap();
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

#[test]
fn indexed_fields_generate_index_sql() {
    let migration = diff_schemas(&Schema::empty(), &Schema::from_model::<IndexedUser>());
    assert!(matches!(
        migration.changes.as_slice(),
        [SchemaChange::CreateTable(model)] if model.table == "indexed_users"
    ));
    let sql = sqlite_migration_sql(&migration);
    assert!(sql.contains("CREATE INDEX IF NOT EXISTS \"indexed_users_email_idx\""));
    assert!(sql.contains("ON \"indexed_users\" (\"email\")"));
}

#[test]
fn index_changes_generate_create_index() {
    let old = Schema::from_models(vec![ModelSchema {
        table: "users".to_string(),
        fields: vec![id_field(), email_field()],
        indexes: Vec::new(),
    }]);
    let new = Schema::from_models(vec![ModelSchema {
        table: "users".to_string(),
        fields: vec![id_field(), email_field()],
        indexes: vec![che_orm::IndexSchema {
            name: "users_email_idx".to_string(),
            columns: vec!["email".to_string()],
            unique: false,
        }],
    }]);
    let migration = diff_schemas(&old, &new);
    assert!(matches!(
        migration.changes.as_slice(),
        [SchemaChange::CreateIndex { table, index }]
            if table == "users" && index.name == "users_email_idx"
    ));
    assert!(sqlite_migration_sql(&migration).contains("CREATE INDEX IF NOT EXISTS"));
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

#[tokio::test]
async fn migration_rebuilds_table_when_field_properties_change() {
    let db = che_orm::SqliteBackend::connect("sqlite::memory:")
        .await
        .unwrap();
    db.apply_sql(
        "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT);\
         INSERT INTO users (email) VALUES ('old@example.com');",
    )
    .await
    .unwrap();

    let mut old_email = email_field();
    old_email.nullable = true;
    let mut new_email = email_field();
    new_email.unique = true;
    new_email.default = Some("'unknown@example.com'".to_string());
    let migration = diff_schemas(
        &Schema::from_models(vec![ModelSchema {
            table: "users".to_string(),
            fields: vec![id_field(), old_email],
            indexes: Vec::new(),
        }]),
        &Schema::from_models(vec![ModelSchema {
            table: "users".to_string(),
            fields: vec![id_field(), new_email],
            indexes: Vec::new(),
        }]),
    );

    db.apply_sql(&sqlite_migration_sql(&migration))
        .await
        .unwrap();

    let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = 1")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(email, "old@example.com");

    let nullable: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('users') WHERE name = 'email' AND \"notnull\" = 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(nullable, 1);
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

fn is_admin_field() -> FieldSchema {
    FieldSchema {
        name: "is_admin".to_string(),
        ty: FieldType::Boolean,
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

fn temporary_path(prefix: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "che_orm_{prefix}_{}.db",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn sqlite_url(path: &std::path::Path) -> String {
    format!("sqlite://{}", path.display())
}
