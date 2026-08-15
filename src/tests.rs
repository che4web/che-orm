#[cfg(feature = "sqlite")]
use crate::models::Post;
use crate::models::User;
use crate::{
    ColumnRef, DatabaseValue, Model, PostgresDialect, QueryBuildError, SqlCompiler, SqliteDialect,
};

#[test]
fn derive_model_defines_consistent_metadata() {
    assert_eq!(User::table_name(), "users");
    assert_eq!(
        User::columns(),
        [
            "id",
            "email",
            "name",
            "is_active",
            "created_at",
            "updated_at"
        ]
    );
    assert_eq!(User::NAME.column(), ColumnRef::new("users", "name"));
}

#[test]
fn application_registry_preserves_app_and_dependency_order() {
    let registry = crate::apps::registry();
    assert_eq!(registry.apps(), ["accounts", "content"]);

    let sql = registry.to_sql::<SqliteDialect>();
    assert!(sql.find("CREATE TABLE users").unwrap() < sql.find("CREATE TABLE posts").unwrap());
}

#[test]
fn schema_and_ast_validation_reject_invalid_metadata() {
    let invalid_schema = crate::TableSchema {
        name: "bad-table",
        columns: Vec::new(),
        unique_constraints: Vec::new(),
        indexes: Vec::new(),
    };
    assert!(matches!(
        invalid_schema.validate(),
        Err(crate::SchemaError::InvalidIdentifier(_))
    ));

    let empty_insert = crate::QueryAst::Insert(crate::InsertAst {
        table: crate::TableRef::new("users"),
        values: Vec::new(),
        returning: Vec::new(),
    });
    assert_eq!(empty_insert.validate(), Err(QueryBuildError::EmptyInsert));
}

#[test]
fn compiles_select_with_sqlite_parameters() {
    let query = User::query()
        .filter(User::ID.gt(10).and(User::IS_ACTIVE.eq(true)))
        .order_by(User::NAME.asc())
        .limit(20)
        .offset(5)
        .into_ast()
        .unwrap();
    let compiled = SqlCompiler::<SqliteDialect>::compile(&query);
    assert_eq!(
        compiled.sql,
        "SELECT users.id, users.email, users.name, users.is_active, users.created_at, users.updated_at FROM users WHERE ((users.id > ?) AND (users.is_active = ?)) ORDER BY users.name ASC LIMIT 20 OFFSET 5"
    );
    assert_eq!(
        compiled.params,
        vec![DatabaseValue::Integer(10), DatabaseValue::Boolean(true)]
    );
}

#[test]
fn compiles_insert_with_postgres_parameters() {
    let query = User::insert()
        .set(User::EMAIL, "alice@example.test")
        .set(User::NAME, "Alice")
        .returning_all()
        .into_ast()
        .unwrap();
    let compiled = SqlCompiler::<PostgresDialect>::compile(&query);
    assert_eq!(
        compiled.sql,
        "INSERT INTO users (email, name) VALUES ($1, $2) RETURNING id, email, name, is_active, created_at, updated_at"
    );
    assert_eq!(
        compiled.params,
        vec![
            DatabaseValue::Text("alice@example.test".into()),
            DatabaseValue::Text("Alice".into())
        ]
    );
}

#[test]
fn rejects_invalid_mutating_queries() {
    assert_eq!(
        User::insert().into_ast().unwrap_err(),
        QueryBuildError::EmptyInsert
    );
    assert_eq!(
        User::update().into_ast().unwrap_err(),
        QueryBuildError::MissingFilter
    );
    assert_eq!(
        User::update()
            .set(User::NAME, "Alice")
            .into_ast()
            .unwrap_err(),
        QueryBuildError::MissingFilter
    );
    assert_eq!(
        User::delete().into_ast().unwrap_err(),
        QueryBuildError::MissingFilter
    );
    assert_eq!(
        User::insert()
            .set(User::NAME, "Alice")
            .set(User::NAME, "Bob")
            .into_ast()
            .unwrap_err(),
        QueryBuildError::DuplicateColumn("name")
    );
}

#[test]
fn compiles_sqlite_create_table_from_model() {
    let compiled = SqlCompiler::<SqliteDialect>::compile(&User::create_table().into_ast());
    let schema = SqlCompiler::<SqliteDialect>::compile_schema(&User::schema());

    assert_eq!(
        compiled.sql,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE, name TEXT NOT NULL CHECK (length(name) > 0), is_active INTEGER NOT NULL DEFAULT true, created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f', 'now')), updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f', 'now')), UNIQUE (email))"
    );
    assert_eq!(
        schema.indexes,
        vec!["CREATE INDEX users_idx_0 ON users (name)"]
    );
}

#[test]
fn orm_update_adds_managed_timestamp() {
    let query = User::update()
        .set(User::NAME, "Updated")
        .filter(User::ID.eq(1))
        .into_ast()
        .unwrap();
    let compiled = SqlCompiler::<SqliteDialect>::compile(&query);

    assert_eq!(
        compiled.sql,
        "UPDATE users SET name = ?, updated_at = ? WHERE (users.id = ?)"
    );
    assert_eq!(compiled.params.len(), 3);
    assert!(matches!(compiled.params[1], DatabaseValue::DateTime(_)));
}

#[test]
fn compiles_postgres_identity_create_table_from_model() {
    let compiled = SqlCompiler::<PostgresDialect>::compile(&User::create_table().into_ast());

    assert_eq!(
        compiled.sql,
        "CREATE TABLE users (id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, email TEXT NOT NULL UNIQUE, name TEXT NOT NULL CHECK (length(name) > 0), is_active BOOLEAN NOT NULL DEFAULT true, created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, UNIQUE (email))"
    );
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite_pool_persists_and_loads_models() {
    let path = std::env::temp_dir().join(format!("che_orm2_test_{}_{}.db", std::process::id(), 1));
    let _ = std::fs::remove_file(&path);

    let database =
        crate::Database::connect_with_pool_size(path.to_string_lossy().into_owned(), 2).unwrap();
    database.create_table::<User>().await.unwrap();

    let user = User {
        id: 0,
        email: "alice@example.test".into(),
        name: "Alice".into(),
        is_active: true,
        created_at: time::OffsetDateTime::now_utc(),
        updated_at: time::OffsetDateTime::now_utc(),
    };
    let result = database.insert(&user).await.unwrap();
    assert_eq!(result.rows_affected, 1);
    assert!(result.last_insert_rowid.unwrap() > 0);

    let loaded = database
        .fetch_one(User::query().filter(User::EMAIL.eq("alice@example.test")))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.email, user.email);
    assert_eq!(loaded.name, user.name);
    assert!(loaded.is_active);

    let previous_updated_at = loaded.updated_at;
    std::thread::sleep(std::time::Duration::from_millis(2));
    database
        .execute(
            User::update()
                .set(User::NAME, "Updated Alice")
                .filter(User::EMAIL.eq("alice@example.test"))
                .into_ast()
                .unwrap(),
        )
        .await
        .unwrap();
    let updated = database
        .fetch_one(User::query().filter(User::EMAIL.eq("alice@example.test")))
        .await
        .unwrap()
        .unwrap();
    assert!(updated.updated_at > previous_updated_at);
    assert_eq!(updated.name, "Updated Alice");

    let orm_updated_at = updated.updated_at;
    std::thread::sleep(std::time::Duration::from_millis(2));
    database
        .transaction(|connection| {
            connection.execute(
                "UPDATE users SET name = ?1 WHERE email = ?2",
                ("Raw SQL Alice", "alice@example.test"),
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let raw_updated = database
        .fetch_one(User::query().filter(User::EMAIL.eq("alice@example.test")))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(raw_updated.name, "Raw SQL Alice");
    assert_eq!(raw_updated.updated_at, orm_updated_at);

    let _ = std::fs::remove_file(path);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite_relations_enforce_foreign_keys_and_fetch_by_field() {
    let path = std::env::temp_dir().join(format!(
        "che_orm2_relation_test_{}_{}.db",
        std::process::id(),
        1
    ));
    let _ = std::fs::remove_file(&path);

    let database =
        crate::Database::connect_with_pool_size(path.to_string_lossy().into_owned(), 2).unwrap();
    database.create_table::<User>().await.unwrap();
    database.create_table::<Post>().await.unwrap();

    let user = User::new("Alice".into());
    let user = User {
        email: "alice@example.test".into(),
        ..user
    };
    let user_id = database
        .insert(&user)
        .await
        .unwrap()
        .last_insert_rowid
        .unwrap();

    database
        .insert(&Post {
            id: 0,
            user_id,
            title: "First post".into(),
        })
        .await
        .unwrap();

    let posts = database.fetch_by(Post::USER_ID, user_id).await.unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].title, "First post");

    let invalid_post = || {
        crate::QueryAst::Insert(crate::InsertAst {
            table: crate::TableRef::new("posts"),
            values: vec![
                crate::InsertValue {
                    column: crate::ColumnRef::new("posts", "user_id"),
                    value: DatabaseValue::Integer(-1),
                },
                crate::InsertValue {
                    column: crate::ColumnRef::new("posts", "title"),
                    value: DatabaseValue::Text("Invalid post".into()),
                },
            ],
            returning: Vec::new(),
        })
    };
    let (invalid_first, invalid_second) = tokio::join!(
        database.execute(invalid_post()),
        database.execute(invalid_post())
    );
    assert!(invalid_first.is_err());
    assert!(invalid_second.is_err());

    database
        .execute(
            User::delete()
                .filter(User::ID.eq(user_id))
                .into_ast()
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        database
            .fetch_by(Post::USER_ID, user_id)
            .await
            .unwrap()
            .is_empty()
    );

    let _ = std::fs::remove_file(path);
}
