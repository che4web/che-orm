use che_orm::{
    ColumnType, DatabaseValue, Model, ModelSerializer, ModelWriteSerializer, PostgresDialect,
    QueryBuildError, SchemaSet, SqlCompiler, SqliteDialect, Uuid, WriteMode,
};

#[derive(Debug, Model)]
#[orm(table = "uuid_users")]
struct User {
    name: String,
    #[orm(primary_key, db_column = "user_uuid")]
    id: Uuid,
}

#[derive(Debug, Model)]
#[orm(table = "uuid_posts")]
struct Post {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = User, on_delete = "cascade")]
    user_id: Uuid,
    #[orm(foreign_key = User)]
    editor_id: Uuid,
    title: String,
}

#[derive(Debug, Model)]
#[orm(table = "uuid_comments")]
struct Comment {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(foreign_key = Post, on_delete = "cascade")]
    post_id: i64,
    #[orm(foreign_key = User)]
    reviewer_id: Uuid,
    body: String,
}

#[derive(Debug, Model)]
#[orm(table = "uuid_optional_posts")]
struct OptionalPost {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(foreign_key = User, on_delete = "set null")]
    user_id: Option<Uuid>,
}

#[derive(ModelSerializer)]
#[serializer(model = User)]
struct UserSerializer {
    #[serializer(read_only)]
    id: Uuid,
    name: String,
}

#[derive(ModelSerializer)]
#[serializer(model = Post)]
struct PostSerializer {
    #[serializer(read_only)]
    id: i64,
    #[serializer(foreign_key = User, relation = PostUserRelation)]
    user_id: Uuid,
    #[serializer(foreign_key = User, relation = PostEditorRelation)]
    editor_id: Uuid,
    title: String,
}

#[derive(ModelSerializer)]
#[serializer(model = Comment)]
#[cfg(feature = "sqlite")]
struct CommentSerializer {
    #[serializer(read_only)]
    id: Uuid,
    post_id: i64,
    body: String,
}

#[derive(ModelSerializer)]
#[serializer(model = Post)]
#[cfg(feature = "sqlite")]
struct PostWithUserSerializer {
    #[serializer(read_only)]
    id: i64,
    #[serializer(one = User, relation = PostUserRelation)]
    user: UserSerializer,
}

#[derive(ModelSerializer)]
#[serializer(model = OptionalPost)]
#[cfg(feature = "sqlite")]
struct OptionalPostSerializer {
    #[serializer(read_only)]
    id: Uuid,
    #[serializer(one = User, relation = OptionalPostUserRelation)]
    user: Option<UserSerializer>,
}

#[derive(ModelSerializer)]
#[serializer(model = Post)]
#[cfg(feature = "sqlite")]
struct PostWithCommentsSerializer {
    #[serializer(read_only)]
    id: i64,
    #[serializer(many = Comment, relation = CommentPostRelation)]
    comments: Vec<CommentSerializer>,
}

#[derive(ModelSerializer)]
#[serializer(model = User)]
#[cfg(feature = "sqlite")]
struct UserWithPostsSerializer {
    #[serializer(read_only)]
    id: Uuid,
    #[serializer(many = Post, relation = PostUserRelation)]
    posts: Vec<PostSerializer>,
}

#[derive(ModelSerializer)]
#[serializer(model = User)]
#[cfg(feature = "sqlite")]
struct UserWithNestedPostsSerializer {
    #[serializer(read_only)]
    id: Uuid,
    #[serializer(many = Post, relation = PostUserRelation)]
    posts: Vec<PostWithCommentsSerializer>,
}

#[test]
fn uuid_schema_and_insert_compile_for_both_dialects() {
    let schema = SchemaSet::new()
        .model::<User>()
        .model::<Post>()
        .model::<Comment>()
        .model::<OptionalPost>();
    schema.validate().unwrap();
    let sqlite = schema.to_sql::<SqliteDialect>();
    assert!(sqlite.contains("user_uuid BLOB PRIMARY KEY NOT NULL"));
    assert!(sqlite.contains("user_id BLOB NOT NULL REFERENCES uuid_users(user_uuid)"));
    let postgres = schema.to_sql::<PostgresDialect>();
    assert!(postgres.contains("user_uuid UUID PRIMARY KEY NOT NULL"));
    assert!(
        !SqlCompiler::<PostgresDialect>::compile(&User::create_table().into_ast())
            .sql
            .contains("IDENTITY")
    );
    assert_eq!(User::schema().columns[1].column_type, ColumnType::Uuid);
    assert!(OptionalPost::schema().columns[1].nullable);
    let ast = User::insert().set(User::NAME, "Alice").into_ast().unwrap();
    let sqlite = SqlCompiler::<SqliteDialect>::compile(&ast);
    let postgres = SqlCompiler::<PostgresDialect>::compile(&ast);
    assert_eq!(
        sqlite.sql,
        "INSERT INTO uuid_users (name, user_uuid) VALUES (?, ?)"
    );
    assert_eq!(
        postgres.sql,
        "INSERT INTO uuid_users (name, user_uuid) VALUES ($1, $2)"
    );
    let DatabaseValue::Uuid(id) = sqlite.params[1] else {
        panic!("missing generated UUID")
    };
    assert_eq!(id.get_version_num(), 7);
    assert_eq!(sqlite.params, postgres.params);
    let explicit = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let ast = User::insert()
        .set(User::ID, explicit)
        .set(User::NAME, "Explicit")
        .into_ast()
        .unwrap();
    let compiled = SqlCompiler::<SqliteDialect>::compile(&ast);
    assert_eq!(compiled.params[0], DatabaseValue::Uuid(explicit));
    assert_eq!(compiled.params.len(), 2);
    assert!(matches!(
        User::insert()
            .set(User::ID, explicit)
            .set(User::ID, explicit)
            .into_ast(),
        Err(QueryBuildError::DuplicateColumn(_))
    ));
    assert!(matches!(
        User::update()
            .set(User::ID, explicit)
            .filter(User::ID.eq(explicit))
            .into_ast(),
        Err(QueryBuildError::PrimaryKeyUpdate)
    ));
    let joined = Post::query().into_joined_ast(Post::USER).unwrap();
    assert!(
        SqlCompiler::<PostgresDialect>::compile(&joined)
            .sql
            .contains("uuid_posts.user_id = user.user_uuid")
    );
}

#[test]
fn uuid_json_and_write_modes_are_typed() {
    let id = Uuid::now_v7();
    let json = UserSerializer::to_json(User {
        id,
        name: "Alice".into(),
    })
    .unwrap();
    assert_eq!(json["id"], id.to_string());
    for mode in [WriteMode::Update { id }, WriteMode::Patch { id }] {
        let write = UserSerializer::is_valid(che_orm::serde_json::json!({"name": "Updated"}), mode)
            .unwrap();
        let che_orm::ValidatedWrite::Update(query) = write else {
            panic!("expected update")
        };
        let compiled = SqlCompiler::<SqliteDialect>::compile(&query.into_ast().unwrap());
        assert_eq!(
            compiled.params,
            vec![
                DatabaseValue::Text("Updated".into()),
                DatabaseValue::Uuid(id)
            ]
        );
    }
    assert!(
        UserSerializer::is_valid(
            che_orm::serde_json::json!({"id": id.to_string(), "name": "Alice"}),
            WriteMode::Create
        )
        .is_err()
    );
    assert!(PostSerializer::is_valid(che_orm::serde_json::json!({"user_id": "invalid", "editor_id": id.to_string(), "title": "bad"}), WriteMode::Create).is_err());
    let write = PostSerializer::is_valid(che_orm::serde_json::json!({"user_id": id.to_string(), "editor_id": id.to_string(), "title": "Good"}), WriteMode::Create).unwrap();
    let che_orm::ValidatedWrite::Create(query) = write else {
        panic!("expected create")
    };
    let compiled = SqlCompiler::<SqliteDialect>::compile(&query.into_ast().unwrap());
    assert_eq!(compiled.params[0], DatabaseValue::Uuid(id));
    assert_eq!(compiled.params[1], DatabaseValue::Uuid(id));
}

#[test]
fn uuid_migration_state_and_sql_preserve_uuid_identity() {
    use che_orm::{Migration, MigrationId, MigrationOperation, ProjectState, SqliteSchemaEditor};
    let state = ProjectState::from_schema(&SchemaSet::new().model::<User>());
    let create = MigrationOperation::CreateTable {
        table: state.tables()[0].clone(),
    };
    let migration = Migration::new(
        MigrationId {
            app: "uuid".into(),
            name: "0001_initial".into(),
        },
        vec![],
        vec![create.clone()],
    );
    assert!(che_orm::render_migration_rust(&migration).contains("ColumnType::Uuid"));
    assert_eq!(migration.calculated_checksum(), migration.checksum);
    let mut altered = migration.clone();
    let MigrationOperation::CreateTable { table } = &mut altered.operations[0] else {
        unreachable!()
    };
    table.columns[1].column_type = ColumnType::Binary;
    assert_ne!(altered.calculated_checksum(), migration.checksum);
    let plan = SqliteSchemaEditor::plan(&create, &che_orm::ProjectState::default()).unwrap();
    assert_eq!(plan.resulting_state, state);
    assert!(plan.statements[0].contains("\"user_uuid\" BLOB PRIMARY KEY NOT NULL"));
    let connection = che_orm::rusqlite::Connection::open_in_memory().unwrap();
    for statement in &plan.statements {
        connection.execute_batch(statement).unwrap();
    }
    assert!(
        connection
            .execute("INSERT INTO uuid_users (name) VALUES ('No key')", [])
            .is_err()
    );
    let id = Uuid::now_v7();
    connection
        .execute(
            "INSERT INTO uuid_users (name, user_uuid) VALUES (?1, ?2)",
            che_orm::rusqlite::params!["Alice", id],
        )
        .unwrap();
    let mut name = state.tables()[0].columns[0].clone();
    name.unique = true;
    let rebuild = SqliteSchemaEditor::plan(
        &MigrationOperation::AlterColumn {
            table: User::table_name().into(),
            old_column: state.tables()[0].columns[0].clone(),
            new_column: name,
        },
        &state,
    )
    .unwrap();
    for statement in &rebuild.statements {
        connection.execute_batch(statement).unwrap();
    }
    let loaded = connection
        .query_row("SELECT name, user_uuid FROM uuid_users", [], User::from_row)
        .unwrap();
    assert_eq!(loaded.id, id);
    assert_eq!(loaded.name, "Alice");
}

#[test]
fn malformed_uuid_blobs_and_text_are_rejected_on_read() {
    let connection = che_orm::rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(&SqlCompiler::<SqliteDialect>::compile(&User::create_table().into_ast()).sql)
        .unwrap();
    for value in [
        che_orm::rusqlite::types::Value::Blob(vec![0; 15]),
        che_orm::rusqlite::types::Value::Text(Uuid::now_v7().to_string()),
    ] {
        connection.execute("DELETE FROM uuid_users", []).unwrap();
        connection
            .execute(
                "INSERT INTO uuid_users (name, user_uuid) VALUES ('Invalid', ?1)",
                [value],
            )
            .unwrap();
        assert!(
            connection
                .query_row("SELECT name, user_uuid FROM uuid_users", [], User::from_row)
                .is_err()
        );
    }
}

#[cfg(feature = "sqlite")]
mod runtime {
    use super::*;
    use che_orm::Database;

    async fn load_related<M, R, Relation>(
        db: &Database,
        relation: che_orm::BelongsTo<M, R, Relation, R::PrimaryKey>,
    ) -> Result<Vec<che_orm::WithOne<M, R, Relation, R::PrimaryKey>>, che_orm::OrmError>
    where
        M: Model + Send + 'static,
        R: Model + Send + 'static,
        Relation: Send + 'static,
    {
        db.query::<M>()
            .select_related(relation)
            .filter(M::primary_key().is_not_null())
            .order_by(M::primary_key().asc())
            .limit(10)
            .offset(0)
            .all(db)
            .await
    }

    async fn load_optional_related<M, R, Relation>(
        db: &Database,
        relation: che_orm::BelongsTo<M, R, Relation, Option<R::PrimaryKey>>,
    ) -> Result<Vec<che_orm::WithOptionalOne<M, R, Relation>>, che_orm::OrmError>
    where
        M: Model + Send + 'static,
        R: Model + Send + 'static,
        Relation: Send + 'static,
    {
        db.query::<M>()
            .select_related(relation)
            .filter(M::primary_key().is_not_null())
            .order_by(M::primary_key().asc())
            .limit(10)
            .offset(0)
            .all(db)
            .await
    }

    fn database() -> Database {
        Database::connect_with_pool_size(":memory:", 1).unwrap()
    }

    #[test]
    fn compiled_uuid_migrations_preserve_blobs_and_foreign_keys_on_rebuild() {
        use che_orm::migration::sqlite_executor::apply_operation_migrations_on_connection;
        use che_orm::{Migration, MigrationGraph, MigrationId, MigrationOperation, ProjectState};
        let state = ProjectState::from_schema(&SchemaSet::new().model::<User>().model::<Post>());
        let initial = Migration::new(
            MigrationId {
                app: "uuid".into(),
                name: "0001_initial".into(),
            },
            vec![],
            state
                .tables()
                .iter()
                .cloned()
                .map(|table| MigrationOperation::CreateTable { table })
                .collect(),
        );
        let mut connection = che_orm::rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON")
            .unwrap();
        assert_eq!(
            apply_operation_migrations_on_connection(
                &mut connection,
                MigrationGraph::new(vec![initial.clone()]).unwrap()
            )
            .unwrap(),
            1
        );
        let id = Uuid::now_v7();
        connection
            .execute(
                "INSERT INTO uuid_users (name, user_uuid) VALUES ('Author', ?1)",
                [id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO uuid_posts (user_id, editor_id, title) VALUES (?1, ?1, 'Post')",
                [id],
            )
            .unwrap();
        let index = Migration::new(
            MigrationId {
                app: "uuid".into(),
                name: "0002_index".into(),
            },
            vec![initial.id.clone()],
            vec![MigrationOperation::AddIndex {
                table: User::table_name().into(),
                columns: vec!["name".into()],
            }],
        );
        let graph = MigrationGraph::new(vec![initial, index]).unwrap();
        assert_eq!(
            apply_operation_migrations_on_connection(&mut connection, graph.clone()).unwrap(),
            1
        );
        assert_eq!(
            apply_operation_migrations_on_connection(&mut connection, graph).unwrap(),
            0
        );
        let user = connection
            .query_row("SELECT name, user_uuid FROM uuid_users", [], User::from_row)
            .unwrap();
        assert_eq!(user.id, id);
        let post = connection
            .query_row(
                "SELECT id, user_id, editor_id, title FROM uuid_posts",
                [],
                Post::from_row,
            )
            .unwrap();
        assert_eq!(post.user_id, id);
        assert_eq!(post.editor_id, id);
        assert!(
            connection
                .execute("INSERT INTO uuid_users (name) VALUES ('Null key')", [])
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "INSERT INTO uuid_posts (user_id, editor_id, title) VALUES (?1, ?1, 'Invalid')",
                    [Uuid::now_v7()]
                )
                .is_err()
        );
    }

    #[tokio::test]
    async fn uuid_prefetch_chunks_large_parent_key_sets() {
        let db = database();
        db.create_table::<User>().await.unwrap();
        db.create_table::<Post>().await.unwrap();
        db.typed_transaction(|tx| {
            for index in 0..901 {
                let user = tx
                    .create::<User>()
                    .set(User::NAME, format!("User {index}"))
                    .execute()?;
                tx.create::<Post>()
                    .set(Post::USER_ID, user.id)
                    .set(Post::EDITOR_ID, user.id)
                    .set(Post::TITLE, "Post")
                    .execute()?;
            }
            Ok(())
        })
        .await
        .unwrap();
        let parents = db
            .query::<User>()
            .prefetch_related(Post::USER.reverse())
            .all(&db)
            .await
            .unwrap();
        assert_eq!(parents.len(), 901);
        for parent in parents {
            assert_eq!(parent.relations.0.related.len(), 1);
            assert_eq!(parent.relations.0.related[0].user_id, parent.model.id);
        }
    }

    #[tokio::test]
    async fn uuid_crud_preserves_keys_and_uses_16_byte_blobs() {
        let db = database();
        db.create_table::<User>().await.unwrap();
        let first = db
            .create::<User>()
            .set(User::NAME, "First")
            .execute()
            .await
            .unwrap();
        let second = db
            .create::<User>()
            .set(User::NAME, "Second")
            .execute()
            .await
            .unwrap();
        assert_eq!(first.id.get_version_num(), 7);
        assert_eq!(second.id.get_version_num(), 7);
        assert_ne!(first.id, second.id);
        let stored = db
            .transaction(|conn| {
                conn.query_row(
                    "SELECT typeof(user_uuid), length(user_uuid) FROM uuid_users LIMIT 1",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
            })
            .await
            .unwrap();
        assert_eq!(stored, ("blob".into(), 16));
        let explicit = Uuid::now_v7();
        db.insert(&User {
            id: explicit,
            name: "Explicit".into(),
        })
        .await
        .unwrap();
        assert_eq!(
            db.get::<User>(explicit).await.unwrap().unwrap().name,
            "Explicit"
        );
        assert!(
            db.insert(&User {
                id: explicit,
                name: "Duplicate".into()
            })
            .await
            .is_err()
        );
        let builder_id = Uuid::now_v7();
        assert_eq!(
            db.create::<User>()
                .set(User::ID, builder_id)
                .set(User::NAME, "Builder")
                .execute()
                .await
                .unwrap()
                .id,
            builder_id
        );
        let updated = db
            .update::<User>(first.id)
            .set(User::NAME, "Updated")
            .execute()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.id, first.id);
        assert_eq!(updated.name, "Updated");
        assert_eq!(
            db.query::<User>()
                .filter(User::ID.in_values([first.id, second.id]))
                .all(&db)
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(db.delete::<User>(first.id).await.unwrap());
        assert!(db.get::<User>(first.id).await.unwrap().is_none());
        assert!(!db.delete::<User>(first.id).await.unwrap());
    }

    #[tokio::test]
    async fn uuid_transactions_and_serializer_writes() {
        let db = database();
        db.create_table::<User>().await.unwrap();
        let id = db
            .typed_transaction(|tx| {
                let user = tx.create::<User>().set(User::NAME, "Initial").execute()?;
                assert_eq!(user.id.get_version_num(), 7);
                tx.update::<User>(user.id)
                    .set(User::NAME, "Committed")
                    .execute()?;
                assert_eq!(tx.get::<User>(user.id)?.unwrap().name, "Committed");
                let supplied = User {
                    id: Uuid::now_v7(),
                    name: "Supplied".into(),
                };
                tx.insert(&supplied)?;
                assert!(tx.get::<User>(supplied.id)?.is_some());
                Ok(user.id)
            })
            .await
            .unwrap();
        let rolled_back = Uuid::now_v7();
        let result: Result<(), che_orm::OrmError> = db
            .typed_transaction(move |tx| {
                tx.create::<User>()
                    .set(User::ID, rolled_back)
                    .set(User::NAME, "Rollback")
                    .execute()?;
                Err(che_orm::rusqlite::Error::InvalidQuery.into())
            })
            .await;
        assert!(result.is_err());
        assert!(db.get::<User>(rolled_back).await.unwrap().is_none());
        let write = UserSerializer::is_valid(
            che_orm::serde_json::json!({"name": "Patched"}),
            WriteMode::Patch { id },
        )
        .unwrap();
        let che_orm::ValidatedWrite::Update(query) = write else {
            panic!("expected update")
        };
        db.execute(query.into_ast().unwrap()).await.unwrap();
        assert_eq!(db.get::<User>(id).await.unwrap().unwrap().name, "Patched");
        let write = UserSerializer::is_valid(
            che_orm::serde_json::json!({"name": "JSON create"}),
            WriteMode::Create,
        )
        .unwrap();
        let che_orm::ValidatedWrite::Create(query) = write else {
            panic!("expected create")
        };
        db.execute(query.into_ast().unwrap()).await.unwrap();
        let user = db
            .fetch_one(User::query().filter(User::NAME.eq("JSON create")))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(user.id.get_version_num(), 7);
    }

    #[tokio::test]
    async fn uuid_relations_load_required_optional_multiple_and_nested_models() {
        let db = database();
        db.create_table::<User>().await.unwrap();
        db.create_table::<Post>().await.unwrap();
        db.create_table::<Comment>().await.unwrap();
        db.create_table::<OptionalPost>().await.unwrap();
        let user = db
            .create::<User>()
            .set(User::NAME, "Author")
            .execute()
            .await
            .unwrap();
        let editor = db
            .create::<User>()
            .set(User::NAME, "Editor")
            .execute()
            .await
            .unwrap();
        let post = db
            .create::<Post>()
            .set(Post::USER_ID, user.id)
            .set(Post::EDITOR_ID, editor.id)
            .set(Post::TITLE, "Post")
            .execute()
            .await
            .unwrap();
        let comment = db
            .create::<Comment>()
            .set(Comment::POST_ID, post.id)
            .set(Comment::REVIEWER_ID, editor.id)
            .set(Comment::BODY, "Comment")
            .execute()
            .await
            .unwrap();
        let optional = db
            .create::<OptionalPost>()
            .set(OptionalPost::USER_ID, Some(user.id))
            .execute()
            .await
            .unwrap();
        db.create::<OptionalPost>()
            .set(OptionalPost::USER_ID, None::<Uuid>)
            .execute()
            .await
            .unwrap();
        let joined = load_related(&db, Post::USER).await.unwrap();
        let json = PostWithUserSerializer::to_json(joined.into_iter().next().unwrap()).unwrap();
        assert_eq!(json["user"]["id"], user.id.to_string());
        let integer_joined = load_related(&db, Comment::POST).await.unwrap();
        assert_eq!(integer_joined[0].related.id, post.id);
        let joined = db
            .query::<Post>()
            .select_related(Post::USER)
            .select_related(Post::EDITOR)
            .all(&db)
            .await
            .unwrap();
        assert_eq!(joined[0].relations.0.related.id, user.id);
        assert_eq!(joined[0].relations.1.related.id, editor.id);
        let mixed = db
            .query::<Comment>()
            .select_related(Comment::POST)
            .select_related(Comment::REVIEWER)
            .all(&db)
            .await
            .unwrap();
        assert_eq!(mixed[0].relations.0.related.id, post.id);
        assert_eq!(mixed[0].relations.1.related.id, editor.id);
        let mixed = db
            .query::<Comment>()
            .select_related(Comment::REVIEWER)
            .select_related(Comment::POST)
            .all(&db)
            .await
            .unwrap();
        assert_eq!(mixed[0].relations.0.related.id, editor.id);
        assert_eq!(mixed[0].relations.1.related.id, post.id);
        let parents = db
            .query::<User>()
            .filter(User::ID.eq(user.id))
            .prefetch_related(Post::USER.reverse())
            .all(&db)
            .await
            .unwrap();
        let json = UserWithPostsSerializer::to_json(parents.into_iter().next().unwrap()).unwrap();
        assert_eq!(json["posts"][0]["user_id"], user.id.to_string());
        let nested = db
            .query::<User>()
            .filter(User::ID.eq(user.id))
            .prefetch_related(Post::USER.reverse().prefetch(Comment::POST.reverse()))
            .all(&db)
            .await
            .unwrap();
        let json =
            UserWithNestedPostsSerializer::to_json(nested.into_iter().next().unwrap()).unwrap();
        assert_eq!(
            json["posts"][0]["comments"][0]["id"],
            comment.id.to_string()
        );
        let multi = db
            .query::<User>()
            .filter(User::ID.eq(user.id))
            .prefetch_related(Post::USER.reverse())
            .prefetch_related(OptionalPost::USER.reverse())
            .all(&db)
            .await
            .unwrap();
        assert_eq!(multi[0].relations.0.related[0].id, post.id);
        assert_eq!(multi[0].relations.1.related[0].id, optional.id);
        let joined = load_optional_related(&db, OptionalPost::USER)
            .await
            .unwrap();
        assert_eq!(joined.len(), 2);
        assert_eq!(joined.iter().filter(|row| row.related.is_none()).count(), 1);
        for row in joined {
            let present = row.related.is_some();
            let json = OptionalPostSerializer::to_json(row).unwrap();
            assert_eq!(!json["user"].is_null(), present);
        }
        assert!(
            db.create::<Post>()
                .set(Post::USER_ID, Uuid::now_v7())
                .set(Post::EDITOR_ID, editor.id)
                .set(Post::TITLE, "Invalid FK")
                .execute()
                .await
                .is_err()
        );
        assert!(db.delete::<User>(user.id).await.unwrap());
        assert!(db.get::<Post>(post.id).await.unwrap().is_none());
        assert!(db.get::<Comment>(comment.id).await.unwrap().is_none());
        assert!(
            db.get::<OptionalPost>(optional.id)
                .await
                .unwrap()
                .unwrap()
                .user_id
                .is_none()
        );
    }
}
