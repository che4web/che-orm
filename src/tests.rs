#[cfg(feature = "sqlite")]
use crate::apps::content::PostUserRelation;
#[cfg(feature = "sqlite")]
use crate::models::Post;
use crate::models::User;
use crate::{
    ColumnRef, DatabaseValue, Model, ModelSerializer, ModelWriteSerializer, PostgresDialect,
    QueryBuildError, SqlCompiler, SqliteDialect,
};

#[cfg(feature = "sqlite")]
#[derive(Debug, Model)]
#[orm(table = "optional_posts")]
struct OptionalPost {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = User, on_delete = "set null")]
    user_id: Option<i64>,
    title: String,
}

#[cfg(feature = "sqlite")]
#[derive(Debug, Model)]
#[orm(table = "audits", index("user_id"))]
struct Audit {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = User, on_delete = "cascade")]
    user_id: i64,
    action: String,
}

#[cfg(feature = "sqlite")]
#[derive(Debug, Model)]
#[orm(table = "comments", index("post_id"))]
struct Comment {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = Post, on_delete = "cascade")]
    post_id: i64,
    body: String,
}

#[cfg(feature = "sqlite")]
#[derive(Debug, Model)]
#[orm(table = "dual_posts")]
struct DualPost {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = User)]
    author_id: i64,
    #[orm(foreign_key = User)]
    editor_id: i64,
    title: String,
}

#[cfg(feature = "sqlite")]
#[derive(ModelSerializer)]
#[serializer(model = OptionalPost)]
struct OptionalPostResponse {
    id: i64,
    #[serializer(one = User, relation = OptionalPostUserRelation)]
    user: Option<UserResponse>,
}

#[derive(ModelSerializer)]
#[serializer(model = User)]
struct UserResponse {
    #[serializer(read_only)]
    id: i64,
    email: String,
    name: String,
    is_active: bool,
    created_at: time::OffsetDateTime,
    updated_at: time::OffsetDateTime,
}

#[derive(ModelSerializer)]
#[serializer(model = User, validate = validate_user_write)]
#[allow(dead_code)]
struct UserWriteSerializer {
    #[serializer(read_only)]
    id: i64,
    #[serializer(write_only)]
    email: String,
    name: String,
    is_active: bool,
    #[serializer(read_only)]
    created_at: time::OffsetDateTime,
    #[serializer(read_only)]
    updated_at: time::OffsetDateTime,
}

fn validate_user_write(
    data: &crate::serde_json::Value,
    _mode: crate::WriteMode,
) -> Result<(), crate::ValidationErrors> {
    if data.get("name").and_then(crate::serde_json::Value::as_str) == Some("") {
        return Err(crate::ValidationErrors {
            detail: "name must not be empty".into(),
        });
    }
    Ok(())
}

#[cfg(feature = "sqlite")]
#[derive(ModelSerializer)]
#[serializer(model = Post)]
struct PostResponse {
    id: i64,
    title: String,
}

#[cfg(feature = "sqlite")]
#[derive(ModelSerializer)]
#[serializer(model = User)]
struct UserWithPostsResponse {
    id: i64,
    name: String,
    #[serializer(many = Post, relation = PostUserRelation)]
    posts: Vec<PostResponse>,
}

#[cfg(feature = "sqlite")]
#[derive(ModelSerializer)]
#[serializer(model = Audit)]
struct AuditResponse {
    id: i64,
    action: String,
}

#[cfg(feature = "sqlite")]
#[derive(ModelSerializer)]
#[serializer(model = User)]
struct UserWithPostsAndAuditsResponse {
    id: i64,
    #[serializer(many = Post, relation = PostUserRelation)]
    posts: Vec<PostResponse>,
    #[serializer(many = Audit, relation = AuditUserRelation)]
    audits: Vec<AuditResponse>,
}

#[cfg(feature = "sqlite")]
#[derive(ModelSerializer)]
#[serializer(model = Comment)]
struct CommentResponse {
    id: i64,
    body: String,
}

#[cfg(feature = "sqlite")]
#[derive(ModelSerializer)]
#[serializer(model = Post)]
struct PostWithCommentsResponse {
    id: i64,
    title: String,
    #[serializer(many = Comment, relation = CommentPostRelation)]
    comments: Vec<CommentResponse>,
}

#[cfg(feature = "sqlite")]
#[derive(ModelSerializer)]
#[serializer(model = User)]
struct UserWithNestedPostsResponse {
    id: i64,
    #[serializer(many = Post, relation = PostUserRelation)]
    posts: Vec<PostWithCommentsResponse>,
}

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
    assert_eq!(User::primary_key().column(), ColumnRef::new("users", "id"));
}

#[test]
fn model_serializer_maps_materialized_models_without_database_access() {
    let user = User::new("Alice".into());
    let response = UserResponse::from_model(user);
    assert_eq!(response.name, "Alice");
    assert_eq!(response.id, 0);

    let responses = UserResponse::many(vec![User::new("Bob".into())]);
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].name, "Bob");
}

#[test]
fn generated_serializer_inputs_are_strict_and_support_patch_presence() {
    let create: UserWriteSerializerCreateInput = crate::serde_json::from_str(
        r#"{"email":"alice@example.test","name":"Alice","is_active":true}"#,
    )
    .unwrap();
    assert_eq!(create.name, "Alice");

    let patch: UserWriteSerializerPatchInput =
        crate::serde_json::from_str(r#"{"name":"Updated","is_active":false}"#).unwrap();
    assert_eq!(patch.email, crate::PatchField::Missing);
    assert_eq!(patch.name, crate::PatchField::Value("Updated".to_string()));
    assert_eq!(patch.is_active, crate::PatchField::Value(false));
    assert_eq!(UserWriteSerializer::fields()[1].write_only, true);

    let unknown = crate::serde_json::from_str::<UserWriteSerializerCreateInput>(
        r#"{"email":"a","name":"A","is_active":true,"id":1}"#,
    );
    assert!(unknown.is_err());

    let invalid = UserWriteSerializer::is_valid(
        crate::serde_json::json!({
            "email": "a@example.test",
            "name": "",
            "is_active": true,
        }),
        crate::WriteMode::Create,
    );
    match invalid {
        Err(error) => assert_eq!(error.detail, "name must not be empty"),
        Ok(_) => panic!("invalid serializer input was accepted"),
    }
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn generated_serializer_rejects_empty_patch_before_database_update() {
    let database = crate::Database::connect_in_memory().unwrap();
    database.create_table::<User>().await.unwrap();
    let result = UserWriteSerializer::is_valid(
        crate::serde_json::json!({}),
        crate::WriteMode::Patch { id: 1 },
    );
    assert!(matches!(result, Err(crate::ValidationErrors { .. })));
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn generated_serializer_executes_create_update_and_patch() {
    let database = crate::Database::connect_in_memory().unwrap();
    database.create_table::<User>().await.unwrap();

    let user = UserWriteSerializer::is_valid(
        crate::serde_json::json!({
            "email": "alice@example.test",
            "name": "Alice",
            "is_active": true,
        }),
        crate::WriteMode::Create,
    )
    .unwrap()
    .save(&database)
    .await
    .unwrap();
    let user = user.unwrap();
    assert_eq!(user.name, "Alice");

    let updated = UserWriteSerializer::is_valid(
        crate::serde_json::json!({
            "email": "alice@example.test",
            "name": "Alice Updated",
            "is_active": false,
        }),
        crate::WriteMode::Update { id: user.id },
    )
    .unwrap()
    .save(&database)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(updated.name, "Alice Updated");
    assert!(!updated.is_active);

    let patched = UserWriteSerializer::is_valid(
        crate::serde_json::json!({"name": "Patched"}),
        crate::WriteMode::Patch { id: user.id },
    )
    .unwrap()
    .save(&database)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(patched.name, "Patched");
    assert!(!patched.is_active);
}

#[cfg(feature = "sqlite")]
#[test]
fn nested_model_serializer_accepts_only_prefetched_result() {
    let user = User::new("Alice".into());
    let result = crate::WithMany {
        model: user,
        related: vec![Post {
            id: 1,
            user_id: 0,
            title: "First".into(),
        }],
        _relation: std::marker::PhantomData,
        _key: std::marker::PhantomData,
    };
    let response = UserWithPostsResponse::from(result);
    assert_eq!(response.posts.len(), 1);
    assert_eq!(response.posts[0].title, "First");

    let responses = UserWithPostsResponse::many(vec![crate::Loaded {
        model: User::new("Bob".into()),
        relations: (crate::LoadedMany {
            related: Vec::new(),
            _relation: std::marker::PhantomData,
        },),
    }]);
    assert_eq!(responses.len(), 1);
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
fn compiles_contains_expression_with_parameter() {
    let query = User::query()
        .filter(User::NAME.contains("Ali"))
        .into_ast()
        .unwrap();
    let compiled = SqlCompiler::<SqliteDialect>::compile(&query);
    assert!(compiled.sql.contains("users.name LIKE ?"));
    assert_eq!(compiled.params, vec![DatabaseValue::Text("%Ali%".into())]);
}

#[test]
fn compiles_in_expression_for_sqlite_and_postgres() {
    let sqlite_query = User::query()
        .filter(
            User::ID
                .in_values([1_i64, 2, 3])
                .and(User::IS_ACTIVE.eq(true)),
        )
        .into_ast()
        .unwrap();
    let sqlite = SqlCompiler::<SqliteDialect>::compile(&sqlite_query);
    assert_eq!(
        sqlite.sql,
        "SELECT users.id, users.email, users.name, users.is_active, users.created_at, users.updated_at FROM users WHERE ((users.id IN (?, ?, ?)) AND (users.is_active = ?))"
    );
    assert_eq!(
        sqlite.params,
        vec![
            DatabaseValue::Integer(1),
            DatabaseValue::Integer(2),
            DatabaseValue::Integer(3),
            DatabaseValue::Boolean(true),
        ]
    );

    let postgres = SqlCompiler::<PostgresDialect>::compile(&sqlite_query);
    assert_eq!(
        postgres.sql,
        "SELECT users.id, users.email, users.name, users.is_active, users.created_at, users.updated_at FROM users WHERE ((users.id IN ($1, $2, $3)) AND (users.is_active = $4))"
    );
}

#[test]
fn compiles_empty_in_expression_as_false() {
    let query = User::query()
        .filter(User::ID.in_values(std::iter::empty::<i64>()))
        .into_ast()
        .unwrap();
    let compiled = SqlCompiler::<SqliteDialect>::compile(&query);
    assert!(compiled.sql.ends_with("WHERE (1 = 0)"));
    assert!(compiled.params.is_empty());
}

#[cfg(feature = "sqlite")]
#[test]
fn compiles_select_related_as_one_join() {
    let query = Post::query().into_joined_ast(Post::USER).unwrap();
    let compiled = SqlCompiler::<SqliteDialect>::compile(&query);
    assert_eq!(
        compiled.sql,
        "SELECT posts.id, posts.user_id, posts.title, user.id, user.email, user.name, user.is_active, user.created_at, user.updated_at FROM posts INNER JOIN users AS user ON (posts.user_id = user.id)"
    );

    let optional_query = OptionalPost::query()
        .into_optional_joined_ast(OptionalPost::USER)
        .unwrap();
    let optional_compiled = SqlCompiler::<SqliteDialect>::compile(&optional_query);
    assert!(optional_compiled.sql.contains("LEFT JOIN users"));
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite_select_related_chains_multiple_aliases() {
    let database = crate::Database::connect_in_memory().unwrap();
    database.create_table::<User>().await.unwrap();
    database.create_table::<DualPost>().await.unwrap();
    let author = database
        .create::<User>()
        .set(User::EMAIL, "author@example.test")
        .set(User::NAME, "Author")
        .set(User::IS_ACTIVE, true)
        .execute()
        .await
        .unwrap();
    let editor = database
        .create::<User>()
        .set(User::EMAIL, "editor@example.test")
        .set(User::NAME, "Editor")
        .set(User::IS_ACTIVE, true)
        .execute()
        .await
        .unwrap();
    database
        .insert(&DualPost {
            id: 0,
            author_id: author.id,
            editor_id: editor.id,
            title: "Aliased".into(),
        })
        .await
        .unwrap();

    let rows = database
        .query::<DualPost>()
        .select_related(DualPost::AUTHOR)
        .select_related(DualPost::EDITOR)
        .all(&database)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].relations.0.related.id, author.id);
    assert_eq!(rows[0].relations.1.related.id, editor.id);

    let filtered = database
        .query::<DualPost>()
        .select_related(DualPost::AUTHOR)
        .filter(DualPost::AUTHOR.related_field(User::NAME).eq("Author"))
        .order_by(DualPost::AUTHOR.related_field(User::NAME).asc())
        .all(&database)
        .await
        .unwrap();
    assert_eq!(filtered.len(), 1);
}

#[test]
fn rejects_in_expression_from_a_foreign_table() {
    let query = User::query()
        .filter(crate::Expr::In {
            left: Box::new(crate::Expr::Column(crate::ColumnRef::new(
                "posts", "user_id",
            ))),
            values: vec![DatabaseValue::Integer(1)],
        })
        .into_ast();
    assert_eq!(
        query.unwrap_err(),
        QueryBuildError::ForeignTableColumn {
            column: "user_id",
            table: "posts",
            expected_table: "users",
        }
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
        User::update()
            .set(User::ID, 2)
            .filter(User::ID.eq(1))
            .into_ast()
            .unwrap_err(),
        QueryBuildError::PrimaryKeyUpdate
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
async fn sqlite_facade_supports_crud_and_typed_queries() {
    let path = std::env::temp_dir().join(format!(
        "che_orm2_facade_test_{}_{}.db",
        std::process::id(),
        1
    ));
    let _ = std::fs::remove_file(&path);

    let database =
        crate::Database::connect_with_pool_size(path.to_string_lossy().into_owned(), 1).unwrap();
    database.create_table::<User>().await.unwrap();

    let user = database
        .create::<User>()
        .set(User::EMAIL, "facade@example.test")
        .set(User::NAME, "Facade")
        .execute()
        .await
        .unwrap();
    assert!(user.id > 0);
    assert_eq!(user.email, "facade@example.test");

    let loaded = database.get::<User>(user.id).await.unwrap().unwrap();
    assert_eq!(loaded.name, "Facade");

    let users = database
        .query::<User>()
        .filter(User::IS_ACTIVE.eq(true))
        .all(&database)
        .await
        .unwrap();
    assert_eq!(users.len(), 1);

    let updated = database
        .update::<User>(user.id)
        .set(User::NAME, "Updated facade")
        .execute()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.name, "Updated facade");
    assert!(
        database
            .update::<User>(-1)
            .set(User::NAME, "Missing")
            .execute()
            .await
            .unwrap()
            .is_none()
    );

    assert!(database.delete::<User>(user.id).await.unwrap());
    assert!(!database.delete::<User>(user.id).await.unwrap());
    assert!(database.get::<User>(user.id).await.unwrap().is_none());

    let _ = std::fs::remove_file(path);
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
    assert_eq!(Post::USER.reverse().related_name(), "post_set");
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
    database.create_table::<Audit>().await.unwrap();
    database.create_table::<Comment>().await.unwrap();

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

    let first_post_id = database
        .insert(&Post {
            id: 0,
            user_id,
            title: "First post".into(),
        })
        .await
        .unwrap()
        .last_insert_rowid
        .unwrap();
    database
        .insert(&Comment {
            id: 0,
            post_id: first_post_id,
            body: "Nice post".into(),
        })
        .await
        .unwrap();
    database
        .insert(&Audit {
            id: 0,
            user_id,
            action: "created".into(),
        })
        .await
        .unwrap();

    let posts = database.fetch_by(Post::USER_ID, user_id).await.unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].title, "First post");

    let post_with_user = database
        .query::<Post>()
        .select_related(Post::USER)
        .all(&database)
        .await
        .unwrap();
    assert_eq!(post_with_user.len(), 1);
    assert_eq!(post_with_user[0].related.id, user_id);

    let second_user = User {
        id: 0,
        email: "bob@example.test".into(),
        name: "Bob".into(),
        is_active: true,
        created_at: time::OffsetDateTime::now_utc(),
        updated_at: time::OffsetDateTime::now_utc(),
    };
    let second_user_id = database
        .insert(&second_user)
        .await
        .unwrap()
        .last_insert_rowid
        .unwrap();
    database
        .insert(&Post {
            id: 0,
            user_id: second_user_id,
            title: "Second post".into(),
        })
        .await
        .unwrap();

    let posts_with_users = database
        .query::<Post>()
        .select_related(Post::USER)
        .all(&database)
        .await
        .unwrap();
    assert_eq!(posts_with_users.len(), 2);
    assert!(posts_with_users.iter().all(|post| post.related.id > 0));

    let users_with_two_relations = database
        .query::<User>()
        .prefetch_related(Post::USER.reverse())
        .prefetch_related(Audit::USER.reverse())
        .all(&database)
        .await
        .unwrap();
    let alice = users_with_two_relations
        .into_iter()
        .find(|user| user.model.id == user_id)
        .unwrap();
    let serialized = UserWithPostsAndAuditsResponse::from(alice);
    assert_eq!(serialized.posts.len(), 1);
    assert_eq!(serialized.audits.len(), 1);

    let nested_users = database
        .query::<User>()
        .prefetch_related(Post::USER.reverse().prefetch(Comment::POST.reverse()))
        .all(&database)
        .await
        .unwrap();
    let nested_user = nested_users
        .into_iter()
        .find(|user| user.model.id == user_id)
        .unwrap();
    let nested_response = UserWithNestedPostsResponse::from(nested_user);
    assert_eq!(nested_response.posts[0].comments.len(), 1);

    let posts = database
        .fetch_by_many(Post::USER_ID, [user_id, second_user_id])
        .await
        .unwrap();
    assert_eq!(posts.len(), 2);
    assert!(posts.iter().any(|post| post.user_id == user_id));
    assert!(posts.iter().any(|post| post.user_id == second_user_id));
    assert!(
        database
            .fetch_by_many(Post::USER_ID, std::iter::empty::<i64>())
            .await
            .unwrap()
            .is_empty()
    );
    let many_ids = (0_i64..1_200).collect::<Vec<_>>();
    let users_by_many_ids = database.fetch_by_many(User::ID, many_ids).await.unwrap();
    assert_eq!(users_by_many_ids.len(), 2);

    let users_with_posts = database
        .query::<User>()
        .prefetch_related(Post::USER.reverse())
        .all(&database)
        .await
        .unwrap();
    let alice = users_with_posts
        .iter()
        .find(|user| user.model.id == user_id)
        .unwrap();
    assert_eq!(alice.relations.0.related.len(), 1);

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

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite_optional_foreign_key_uses_left_join_and_set_null() {
    let database = crate::Database::connect_in_memory().unwrap();
    database.create_table::<User>().await.unwrap();
    database.create_table::<OptionalPost>().await.unwrap();
    let user = database
        .create::<User>()
        .set(User::EMAIL, "optional@example.test")
        .set(User::NAME, "Optional")
        .set(User::IS_ACTIVE, true)
        .execute()
        .await
        .unwrap();

    database
        .insert(&OptionalPost {
            id: 0,
            user_id: Some(user.id),
            title: "Attached".into(),
        })
        .await
        .unwrap();
    database
        .insert(&OptionalPost {
            id: 0,
            user_id: None,
            title: "Detached".into(),
        })
        .await
        .unwrap();

    let rows = database
        .query::<OptionalPost>()
        .select_related(OptionalPost::USER)
        .all(&database)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.iter().filter(|row| row.related.is_some()).count(), 1);
    assert_eq!(rows.iter().filter(|row| row.related.is_none()).count(), 1);
    let serialized = OptionalPostResponse::many(rows);
    assert_eq!(serialized.len(), 2);

    assert!(database.delete::<User>(user.id).await.unwrap());
    let detached = database
        .all::<OptionalPost>()
        .await
        .unwrap()
        .into_iter()
        .find(|post| post.title == "Attached")
        .unwrap();
    assert_eq!(detached.user_id, None);
}
