#![allow(dead_code)]

use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(feature = "postgres")]
use che_orm::PostgresBackend;
use che_orm::{
    Application, Database, DatabaseSettings, Error, FieldSchema, FieldType, ForeignKeyAction,
    ForeignKeySchema, IndexSchema, Manager, MigrationOptions, MigrationSettings, Model,
    ModelSchema, Result, RuntimeSettings, Schema, SchemaChange, diff_schemas, postgres_schema_sql,
    sqlite_migration_sql, validate_migration,
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
#[model(table = "che_orm_postgres_users")]
struct PostgresUser {
    #[field(primary_key, rename = "user_id")]
    id: i64,
    email: String,
    metadata: serde_json::Value,
    active: Option<bool>,
}

struct TestApplication {
    migrations_dir: std::path::PathBuf,
}

fn temporary_migrations_dir(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "che_orm_{name}_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

impl Application for TestApplication {
    fn schema(&self) -> Schema {
        Schema::from_model::<User>()
    }

    fn settings(&self) -> Result<RuntimeSettings> {
        Ok(RuntimeSettings {
            database: DatabaseSettings {
                url: "sqlite::memory:".to_string(),
            },
            migrations: MigrationSettings {
                dir: self.migrations_dir.clone(),
            },
        })
    }
}

#[tokio::test]
async fn namespaced_migrations_isolate_app_versions_and_status() {
    let auth_dir = temporary_migrations_dir("auth_migrations");
    let tasks_dir = temporary_migrations_dir("tasks_migrations");
    fs::create_dir_all(&auth_dir).unwrap();
    fs::create_dir_all(&tasks_dir).unwrap();
    fs::write(
        auth_dir.join("0001_initial.sql"),
        "CREATE TABLE auth_records (id INTEGER PRIMARY KEY);",
    )
    .unwrap();
    fs::write(
        tasks_dir.join("0001_initial.sql"),
        "CREATE TABLE task_records (id INTEGER PRIMARY KEY);",
    )
    .unwrap();

    let db = Database::connect("sqlite::memory:").await.unwrap();
    assert_eq!(
        db.apply_migrations_dir_with_namespace("auth", &auth_dir)
            .await
            .unwrap(),
        ["initial"]
    );
    assert_eq!(
        db.apply_migrations_dir_with_namespace("tasks", &tasks_dir)
            .await
            .unwrap(),
        ["initial"]
    );
    assert!(
        db.apply_migrations_dir_with_namespace("auth", &auth_dir)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        db.apply_migrations_dir_with_namespace("tasks", &tasks_dir)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'auth_records')"
    )
    .fetch_one(db.pool())
    .await
    .unwrap());
    assert!(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'task_records')"
    )
    .fetch_one(db.pool())
    .await
    .unwrap());
    assert!(
        db.migration_status_with_namespace("auth", &auth_dir)
            .await
            .unwrap()
            .iter()
            .all(|migration| migration.applied && !migration.checksum_mismatch)
    );
    assert!(
        db.migration_status_with_namespace("tasks", &tasks_dir)
            .await
            .unwrap()
            .iter()
            .all(|migration| migration.applied && !migration.checksum_mismatch)
    );

    fs::write(
        auth_dir.join("0001_initial.sql"),
        "CREATE TABLE auth_records (id INTEGER PRIMARY KEY, changed TEXT);",
    )
    .unwrap();
    assert!(
        db.apply_migrations_dir_with_namespace("auth", &auth_dir)
            .await
            .is_err()
    );

    fs::remove_dir_all(auth_dir).unwrap();
    fs::remove_dir_all(tasks_dir).unwrap();
}

#[test]
fn runtime_settings_load_migration_engine_from_toml() {
    let path = std::env::temp_dir().join(format!(
        "che_orm_settings_{}.toml",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::write(
        &path,
        "[database]\nurl = \"sqlite::memory:\"\n\n[migrations]\ndir = \"db_migrations\"\n",
    )
    .unwrap();
    let settings = RuntimeSettings::load(&path).unwrap();
    assert_eq!(settings.database.url, "sqlite::memory:");
    assert_eq!(
        settings.migrations.dir,
        std::path::Path::new("db_migrations")
    );
    fs::remove_file(path).unwrap();
}

#[test]
fn postgres_schema_uses_postgres_types_and_identity_columns() {
    let sql = postgres_schema_sql(&Schema::from_model::<User>());
    assert!(sql.contains("BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY"));
    assert!(sql.contains("email"));
    assert!(sql.contains("BOOLEAN NOT NULL DEFAULT true"));
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_manual_migrations_are_supported_when_configured() {
    let Some(database_url) = std::env::var_os("CHE_ORM_TEST_POSTGRES_URL") else {
        return;
    };
    let migrations_dir = std::env::temp_dir().join(format!(
        "che_orm_postgres_migrations_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&migrations_dir).unwrap();
    fs::write(
        migrations_dir.join("20260101000000_test.sql"),
        "CREATE TABLE IF NOT EXISTS che_orm_test (id BIGINT PRIMARY KEY);",
    )
    .unwrap();
    let backend = PostgresBackend::connect(database_url.to_str().unwrap())
        .await
        .unwrap();
    backend.migrate(&migrations_dir).await.unwrap();
    fs::remove_dir_all(migrations_dir).unwrap();
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_model_manager_supports_create_get_and_all_when_configured() {
    let Some(database_url) = std::env::var_os("CHE_ORM_TEST_POSTGRES_URL") else {
        return;
    };
    let backend = PostgresBackend::connect(database_url.to_str().unwrap())
        .await
        .unwrap();
    sqlx::query("DROP TABLE IF EXISTS che_orm_postgres_users")
        .execute(backend.pool())
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE che_orm_postgres_users (user_id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, email TEXT NOT NULL, metadata JSONB NOT NULL, active BOOLEAN)",
    )
    .execute(backend.pool())
    .await
    .unwrap();

    let database = Database::connect(database_url.to_str().unwrap())
        .await
        .unwrap();
    let created = database
        .create::<PostgresUser>()
        .set(PostgresUserFields::EMAIL, "postgres@example.com")
        .set(
            PostgresUserFields::METADATA,
            serde_json::json!({"backend": "postgres"}),
        )
        .set(PostgresUserFields::ACTIVE, true)
        .execute()
        .await
        .unwrap();
    assert_eq!(created.id, 1);
    assert_eq!(
        database
            .get::<PostgresUser>(created.id)
            .await
            .unwrap()
            .email,
        created.email
    );
    let updated = database
        .update::<PostgresUser>(created.id)
        .set(PostgresUserFields::EMAIL, "updated@example.com")
        .set(PostgresUserFields::ACTIVE, None::<bool>)
        .execute()
        .await
        .unwrap();
    assert_eq!(updated.email, "updated@example.com");
    assert_eq!(updated.active, None);
    let saved = PostgresUser {
        email: "saved@example.com".to_string(),
        ..updated
    };
    assert_eq!(
        database.save(&saved).await.unwrap().email,
        "saved@example.com"
    );
    let queried = database
        .query::<PostgresUser>()
        .filter(
            PostgresUserFields::EMAIL
                .contains("saved")
                .and(PostgresUserFields::ACTIVE.is_null()),
        )
        .order_by_desc(PostgresUserFields::ID)
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(queried.email, "saved@example.com");
    assert_eq!(
        database
            .query::<PostgresUser>()
            .filter(PostgresUserFields::EMAIL.in_values(["saved@example.com"]))
            .offset(1)
            .count()
            .await
            .unwrap(),
        1
    );
    assert_eq!(database.all::<PostgresUser>().await.unwrap().len(), 1);
    database.delete::<PostgresUser>(created.id).await.unwrap();
    assert!(database.all::<PostgresUser>().await.unwrap().is_empty());

    sqlx::query("DROP TABLE che_orm_postgres_users")
        .execute(backend.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn application_manager_uses_application_schema_and_settings() {
    let migrations_dir = std::env::temp_dir().join(format!(
        "che_orm_manager_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let manager = Manager::new(TestApplication {
        migrations_dir: migrations_dir.clone(),
    });

    assert!(manager.makemigrations("initial").unwrap().path.is_some());
    assert_eq!(manager.migrate().await.unwrap(), vec!["initial"]);

    fs::remove_dir_all(migrations_dir).unwrap();
}

#[tokio::test]
async fn database_facade_generates_and_applies_migrations() {
    let migrations_dir = std::env::temp_dir().join(format!(
        "che_orm_database_facade_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let database = Database::connect("sqlite::memory:")
        .await
        .unwrap()
        .with_migrations_dir(&migrations_dir);
    let schema = Schema::from_model::<User>();

    let generated = database
        .makemigrations(
            &schema,
            MigrationOptions::new(&migrations_dir).named("initial"),
        )
        .unwrap();
    assert!(generated.path.is_some());
    assert_eq!(database.migrate().await.unwrap(), vec!["initial"]);
    assert!(database.all::<User>().await.unwrap().is_empty());
    let created = database
        .create::<User>()
        .set(UserFields::EMAIL, "shortcut@example.com")
        .execute()
        .await
        .unwrap();
    assert_eq!(created.email, "shortcut@example.com");
    assert_eq!(
        database.get::<User>(created.id).await.unwrap().email,
        "shortcut@example.com"
    );
    let updated = database
        .update::<User>(created.id)
        .set(UserFields::EMAIL, "updated@example.com")
        .execute()
        .await
        .unwrap();
    assert_eq!(updated.email, "updated@example.com");
    let saved = database
        .save(&User {
            email: "saved@example.com".to_string(),
            ..updated
        })
        .await
        .unwrap();
    assert_eq!(saved.email, "saved@example.com");
    assert_eq!(database.all::<User>().await.unwrap().len(), 1);
    database.delete::<User>(saved.id).await.unwrap();
    assert!(database.all::<User>().await.unwrap().is_empty());

    std::fs::remove_dir_all(migrations_dir).unwrap();
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
fn type_changes_require_explicit_data_conversion() {
    let old = Schema::from_models(vec![ModelSchema {
        table: "users".to_string(),
        fields: vec![id_field(), email_field()],
        indexes: Vec::new(),
    }]);
    let mut new_email = email_field();
    new_email.ty = FieldType::Integer;
    let new = Schema::from_models(vec![ModelSchema {
        table: "users".to_string(),
        fields: vec![id_field(), new_email],
        indexes: Vec::new(),
    }]);

    let error = validate_migration(&diff_schemas(&old, &new)).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("requires an explicit data conversion")
    );
}

#[test]
fn destructive_changes_are_marked_in_sql() {
    let old = Schema::from_model::<User>();
    let new = Schema::empty();
    let sql = sqlite_migration_sql(&diff_schemas(&old, &new));
    assert!(sql.contains("-- che-orm: destructive"));
    assert!(sql.contains("DROP TABLE IF EXISTS users;"));
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
    let db = Database::connect("sqlite::memory:").await.unwrap();
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
    let db = Database::connect("sqlite::memory:").await.unwrap();
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
        let db = Database::connect("sqlite::memory:").await.unwrap();
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
    let db = Database::connect(&sqlite_url(&path)).await.unwrap();
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
        "CREATE TABLE IF NOT EXISTS migrated (id INTEGER PRIMARY KEY);",
    )
    .unwrap();

    let url = sqlite_url(&path);
    let first = Database::connect(&url).await.unwrap();
    let second = Database::connect(&url).await.unwrap();
    let (first_result, second_result) = tokio::join!(
        first.apply_migrations_dir(&migrations_dir),
        second.apply_migrations_dir(&migrations_dir),
    );
    first_result.unwrap();
    second_result.unwrap();

    let migrations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
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
async fn migration_replaces_changed_index_with_same_name() {
    let old = Schema::from_models(vec![ModelSchema {
        table: "users".to_string(),
        fields: vec![id_field(), email_field()],
        indexes: vec![IndexSchema {
            name: "users_email_idx".to_string(),
            columns: vec!["email".to_string()],
            unique: false,
        }],
    }]);
    let new = Schema::from_models(vec![ModelSchema {
        table: "users".to_string(),
        fields: vec![id_field(), email_field()],
        indexes: vec![IndexSchema {
            name: "users_email_idx".to_string(),
            columns: vec!["email".to_string()],
            unique: true,
        }],
    }]);
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.apply_sql(&sqlite_migration_sql(&diff_schemas(&Schema::empty(), &old)))
        .await
        .unwrap();

    let sql = sqlite_migration_sql(&diff_schemas(&old, &new));
    assert!(sql.find("DROP INDEX").unwrap() < sql.find("CREATE UNIQUE INDEX").unwrap());
    db.apply_sql(&sql).await.unwrap();

    let index_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'users_email_idx'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(index_sql.starts_with("CREATE UNIQUE INDEX"));
}

#[tokio::test]
async fn migration_rebuilds_table_when_column_is_removed() {
    let db = che_orm::Database::connect("sqlite::memory:").await.unwrap();
    db.create_table::<OldUser>().await.unwrap();
    let old_user = db
        .create::<OldUser>()
        .set(OldUserFields::EMAIL, "removed@example.com")
        .set(OldUserFields::NAME, "Alice")
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

    let current_user = db.get::<CurrentUser>(old_user.id).await.unwrap();
    assert_eq!(current_user.name, "Alice");
    fs::remove_dir_all(migrations_dir).unwrap();
}

#[tokio::test]
async fn migration_preflight_rejects_duplicate_unique_values() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.apply_sql(
        "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT);\
         INSERT INTO users (email) VALUES ('duplicate@example.com'), ('duplicate@example.com');",
    )
    .await
    .unwrap();
    let old = Schema::from_models(vec![ModelSchema {
        table: "users".to_string(),
        fields: vec![id_field(), email_field()],
        indexes: Vec::new(),
    }]);
    let mut new_email = email_field();
    new_email.unique = true;
    let new = Schema::from_models(vec![ModelSchema {
        table: "users".to_string(),
        fields: vec![id_field(), new_email],
        indexes: Vec::new(),
    }]);

    let error = db
        .apply_sql(&sqlite_migration_sql(&diff_schemas(&old, &new)))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::MigrationPreflightFailed { .. }));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn migration_preflight_rejects_choice_and_length_violations() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.apply_sql("CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT); INSERT INTO users (email) VALUES ('too-long');")
        .await
        .unwrap();
    let old = Schema::from_models(vec![ModelSchema {
        table: "users".to_string(),
        fields: vec![id_field(), email_field()],
        indexes: Vec::new(),
    }]);
    let mut new_email = email_field();
    new_email.max_length = Some(3);
    let new = Schema::from_models(vec![ModelSchema {
        table: "users".to_string(),
        fields: vec![id_field(), new_email],
        indexes: Vec::new(),
    }]);
    let error = db
        .apply_sql(&sqlite_migration_sql(&diff_schemas(&old, &new)))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::MigrationPreflightFailed { .. }));

    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.apply_sql("CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT); INSERT INTO users (email) VALUES ('legacy');")
        .await
        .unwrap();
    let mut new_email = email_field();
    new_email.choices = Some(vec!["active".to_string(), "disabled".to_string()]);
    let new = Schema::from_models(vec![ModelSchema {
        table: "users".to_string(),
        fields: vec![id_field(), new_email],
        indexes: Vec::new(),
    }]);
    let error = db
        .apply_sql(&sqlite_migration_sql(&diff_schemas(&old, &new)))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::MigrationPreflightFailed { .. }));
}

#[tokio::test]
async fn migration_rebuilds_table_when_field_properties_change() {
    let db = che_orm::Database::connect("sqlite::memory:").await.unwrap();
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
