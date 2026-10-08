//! Identical ORM scenarios run against the compile-time selected backend.
use che_orm::{
    Database, DbEnum, ImageField, Migration, MigrationGraph, MigrationId, MigrationOperation,
    Model, ModelSerializer, ModelWriteSerializer, OrmError, ProjectState, SchemaSet, Uuid,
    WriteMode,
};
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, DbEnum)]
enum Status {
    Draft,
    Published,
}

#[derive(Debug, Model)]
#[orm(table = "portable_accounts")]
struct Account {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(unique)]
    name: String,
    score: f64,
    payload: Vec<u8>,
    enabled: bool,
    status: Status,
    image: Option<ImageField>,
    optional_count: Option<i64>,
    optional_score: Option<f64>,
    optional_uuid: Option<Uuid>,
    optional_text: Option<String>,
    optional_blob: Option<Vec<u8>>,
    optional_bool: Option<bool>,
    optional_status: Option<Status>,
    due: Option<OffsetDateTime>,
    #[orm(auto_now_add)]
    created: OffsetDateTime,
    #[orm(auto_now)]
    updated: OffsetDateTime,
}
#[derive(Debug, Model)]
#[orm(table = "portable_posts")]
struct Post {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key=Account,on_delete="cascade")]
    user_id: Uuid,
    #[orm(foreign_key=Account)]
    editor_id: Uuid,
    title: String,
}
#[derive(Debug, Model)]
#[orm(table = "portable_comments")]
struct Comment {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(foreign_key=Post,on_delete="cascade")]
    post_id: i64,
    body: String,
}
#[derive(Debug, Model)]
#[orm(table = "portable_optional_posts")]
struct OptionalPost {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(foreign_key=Account,on_delete="set null")]
    user_id: Option<Uuid>,
}
#[derive(ModelSerializer)]
#[serializer(model=Account)]
struct AccountSerializer {
    #[serializer(read_only)]
    id: Uuid,
    name: String,
    score: f64,
    payload: Vec<u8>,
    enabled: bool,
    status: Status,
    image: Option<ImageField>,
    due: Option<OffsetDateTime>,
    #[serializer(read_only)]
    created: OffsetDateTime,
}
#[derive(ModelSerializer)]
#[serializer(model=Comment)]
struct CommentSerializer {
    #[serializer(read_only)]
    id: Uuid,
    body: String,
}
#[derive(ModelSerializer)]
#[serializer(model=Post)]
struct PostWithComments {
    #[serializer(read_only)]
    id: i64,
    #[serializer(many=Comment,relation=CommentPostRelation)]
    comments: Vec<CommentSerializer>,
}
#[derive(ModelSerializer)]
#[serializer(model=Account)]
struct AccountWithPosts {
    #[serializer(read_only)]
    id: Uuid,
    #[serializer(many=Post,relation=PostUserRelation)]
    posts: Vec<PostWithComments>,
}
#[derive(ModelSerializer)]
#[serializer(model=OptionalPost)]
struct OptionalWithAccount {
    #[serializer(read_only)]
    id: Uuid,
    #[serializer(one=Account,relation=OptionalPostUserRelation)]
    user: Option<AccountSerializer>,
}

#[derive(Debug, Model)]
#[orm(table = "portable_migration_parents", index("name"))]
struct MigrationParent {
    #[orm(primary_key)]
    id: Uuid,
    name: String,
    #[orm(foreign_key=MigrationChild,on_delete="set null")]
    child_id: Option<Uuid>,
}
#[derive(Debug, Model)]
#[orm(table = "portable_migration_children")]
struct MigrationChild {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(foreign_key=MigrationParent,on_delete="cascade")]
    parent_id: Uuid,
}

async fn fixture() -> (Database, Option<(Database, String)>) {
    #[cfg(feature = "sqlite")]
    {
        (Database::connect(":memory:").await.unwrap(), None)
    }
    #[cfg(feature = "postgres")]
    {
        let url = std::env::var("TEST_POSTGRES_URL")
            .expect("set TEST_POSTGRES_URL to run PostgreSQL runtime tests");
        let admin = Database::connect(url.clone()).await.unwrap();
        let schema = format!("orm_test_{}", Uuid::now_v7().simple());
        admin
            .execute_sql(format!("CREATE SCHEMA \"{schema}\""), vec![])
            .await
            .unwrap();
        let separator = if url.contains('?') { '&' } else { '?' };
        let scoped = format!("{url}{separator}options=-csearch_path%3D{schema}");
        (
            Database::connect_with_pool_size(scoped, 1).await.unwrap(),
            Some((admin, schema)),
        )
    }
}
async fn account(db: &Database, name: &str) -> Account {
    db.create::<Account>()
        .set(Account::NAME, name)
        .set(Account::SCORE, 12.75)
        .set(Account::PAYLOAD, vec![0, 1, 255])
        .set(Account::ENABLED, true)
        .set(Account::STATUS, Status::Draft)
        .execute()
        .await
        .unwrap()
}
async fn required<M, R, Relation>(
    db: &Database,
    relation: che_orm::BelongsTo<M, R, Relation, R::PrimaryKey>,
) -> Vec<che_orm::WithOne<M, R, Relation, R::PrimaryKey>>
where
    M: Model + Send + 'static,
    R: Model + Send + 'static,
{
    db.query::<M>()
        .select_related(relation)
        .all(db)
        .await
        .unwrap()
}
async fn optional<M, R, Relation>(
    db: &Database,
    relation: che_orm::BelongsTo<M, R, Relation, Option<R::PrimaryKey>>,
) -> Vec<che_orm::WithOptionalOne<M, R, Relation>>
where
    M: Model + Send + 'static,
    R: Model + Send + 'static,
{
    db.query::<M>()
        .select_related(relation)
        .all(db)
        .await
        .unwrap()
}

#[tokio::test]
#[cfg_attr(
    feature = "postgres",
    ignore = "requires TEST_POSTGRES_URL and a live PostgreSQL server"
)]
async fn portable_runtime_crud_transactions_relations_and_migrations() {
    let (db, cleanup) = fixture().await;
    #[cfg(feature = "postgres")]
    {
        use che_orm::DatabaseValue as V;
        let row = db
            .query_sql(
                "SELECT $1::int2, $2::int4, $3::float4, $4::int2, $5::float4, $6::name",
                vec![
                    V::Integer(12),
                    V::Integer(123456),
                    V::Real(1.25),
                    V::Null,
                    V::Null,
                    V::Null,
                ],
            )
            .await
            .unwrap()
            .remove(0);
        assert_eq!(row.get::<i64>(0).unwrap(), 12);
        assert_eq!(row.get::<i64>(1).unwrap(), 123456);
        assert_eq!(row.get::<f64>(2).unwrap(), 1.25);
        assert!(row.get::<Option<i64>>(3).unwrap().is_none());
        assert!(row.get::<Option<f64>>(4).unwrap().is_none());
        assert!(row.get::<Option<String>>(5).unwrap().is_none());
        assert!(
            db.query_sql("SELECT $1::int2", vec![V::Integer(i64::MAX)])
                .await
                .is_err()
        );
        assert!(
            db.query_sql("SELECT $1::float4", vec![V::Real(f64::MAX)])
                .await
                .is_err()
        );
    }
    db.create_table::<Account>().await.unwrap();
    db.create_table::<Post>().await.unwrap();
    db.create_table::<Comment>().await.unwrap();
    db.create_table::<OptionalPost>().await.unwrap();
    let alice = account(&db, "Alice").await;
    let bob = account(&db, "Bob").await;
    assert_eq!(alice.id.get_version_num(), 7);
    assert_ne!(alice.id, bob.id);
    assert_eq!(alice.score, 12.75);
    assert_eq!(alice.payload, vec![0, 1, 255]);
    assert!(alice.enabled);
    assert_eq!(alice.status, Status::Draft);
    assert!(alice.optional_count.is_none());
    assert!(alice.due.is_none());
    let due = OffsetDateTime::parse(
        "2026-10-09T12:34:56+05:00",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let updated = db
        .update::<Account>(alice.id)
        .set(Account::OPTIONAL_COUNT, Some(-42_i64))
        .set(Account::OPTIONAL_SCORE, Some(-0.125))
        .set(Account::OPTIONAL_UUID, Some(bob.id))
        .set(Account::OPTIONAL_TEXT, Some("O'Reilly; -- text".to_owned()))
        .set(Account::OPTIONAL_BLOB, Some(vec![0, 255]))
        .set(Account::OPTIONAL_BOOL, Some(false))
        .set(Account::OPTIONAL_STATUS, Some(Status::Published))
        .set(Account::IMAGE, Some(ImageField::new("avatars/alice.png")))
        .set(Account::DUE, Some(due))
        .execute()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.optional_count, Some(-42));
    assert_eq!(updated.optional_score, Some(-0.125));
    assert_eq!(updated.optional_uuid, Some(bob.id));
    assert_eq!(updated.optional_text.as_deref(), Some("O'Reilly; -- text"));
    assert_eq!(updated.optional_blob, Some(vec![0, 255]));
    assert_eq!(updated.optional_bool, Some(false));
    assert_eq!(updated.optional_status, Some(Status::Published));
    assert_eq!(updated.due, Some(due));
    assert!(updated.updated >= alice.updated);
    let json = AccountSerializer::to_json(updated).unwrap();
    assert_eq!(json["id"], alice.id.to_string());
    assert_eq!(json["image"], "avatars/alice.png");
    assert!(json["created"].as_str().unwrap().ends_with('Z'));
    let patch = AccountSerializer::is_valid(
        che_orm::serde_json::json!({"due":null}),
        WriteMode::Patch { id: alice.id },
    )
    .unwrap();
    assert!(patch.save(&db).await.unwrap().unwrap().due.is_none());
    let nulls = db
        .update::<Account>(alice.id)
        .set(Account::OPTIONAL_COUNT, None::<i64>)
        .set(Account::OPTIONAL_SCORE, None::<f64>)
        .set(Account::OPTIONAL_UUID, None::<Uuid>)
        .set(Account::OPTIONAL_TEXT, None::<String>)
        .set(Account::OPTIONAL_BLOB, None::<Vec<u8>>)
        .set(Account::OPTIONAL_BOOL, None::<bool>)
        .set(Account::OPTIONAL_STATUS, None::<Status>)
        .set(Account::IMAGE, None::<ImageField>)
        .execute()
        .await
        .unwrap()
        .unwrap();
    assert!(nulls.optional_count.is_none());
    assert!(nulls.optional_score.is_none());
    assert!(nulls.optional_uuid.is_none());
    assert!(nulls.optional_text.is_none());
    assert!(nulls.optional_blob.is_none());
    assert!(nulls.optional_bool.is_none());
    assert!(nulls.optional_status.is_none());
    assert!(nulls.image.is_none());
    assert_eq!(db.count::<Account>().await.unwrap(), 2);
    assert_eq!(
        db.query::<Account>()
            .filter(Account::SCORE.gt(12.5))
            .limit(1)
            .count(&db)
            .await
            .unwrap(),
        1
    );
    let explicit = Uuid::now_v7();
    db.insert(&Account {
        id: explicit,
        name: "Supplied".into(),
        score: 1.5,
        payload: vec![],
        enabled: false,
        status: Status::Published,
        image: None,
        optional_count: None,
        optional_score: None,
        optional_uuid: None,
        optional_text: None,
        optional_blob: None,
        optional_bool: None,
        optional_status: None,
        due: None,
        created: OffsetDateTime::now_utc(),
        updated: OffsetDateTime::now_utc(),
    })
    .await
    .unwrap();
    assert!(db.get::<Account>(explicit).await.unwrap().is_some());
    assert!(db.delete::<Account>(explicit).await.unwrap());
    let post = db
        .create::<Post>()
        .set(Post::USER_ID, alice.id)
        .set(Post::EDITOR_ID, bob.id)
        .set(Post::TITLE, "First")
        .execute()
        .await
        .unwrap();
    assert!(post.id > 0);
    let comment = db
        .create::<Comment>()
        .set(Comment::POST_ID, post.id)
        .set(Comment::BODY, "Hello")
        .execute()
        .await
        .unwrap();
    let mut many_keys = vec![alice.id; 60_001];
    many_keys.push(bob.id);
    assert_eq!(
        db.fetch_by_many(Post::USER_ID, many_keys)
            .await
            .unwrap()
            .len(),
        1
    );
    let attached = db
        .create::<OptionalPost>()
        .set(OptionalPost::USER_ID, Some(alice.id))
        .execute()
        .await
        .unwrap();
    db.create::<OptionalPost>()
        .set(OptionalPost::USER_ID, None::<Uuid>)
        .execute()
        .await
        .unwrap();
    assert_eq!(required(&db, Post::USER).await[0].related.id, alice.id);
    assert_eq!(required(&db, Comment::POST).await[0].related.id, post.id);
    let alias = Post::USER.related_field(Account::NAME);
    assert_eq!(
        db.query::<Post>()
            .select_related(Post::USER)
            .filter(alias.eq("Alice"))
            .count(&db)
            .await
            .unwrap(),
        1
    );
    let alias = Post::USER.related_field(Account::NAME);
    assert_eq!(
        db.query::<Post>()
            .select_related(Post::USER)
            .filter(alias.eq("Alice"))
            .order_by(Post::USER.related_field(Account::NAME).asc())
            .all(&db)
            .await
            .unwrap()
            .len(),
        1
    );
    let multiple = db
        .query::<Post>()
        .select_related(Post::USER)
        .select_related(Post::EDITOR)
        .all(&db)
        .await
        .unwrap();
    assert_eq!(multiple[0].relations.0.related.id, alice.id);
    assert_eq!(multiple[0].relations.1.related.id, bob.id);
    let optional_rows = optional(&db, OptionalPost::USER).await;
    assert_eq!(
        optional_rows
            .iter()
            .filter(|row| row.related.is_none())
            .count(),
        1
    );
    for row in optional_rows {
        OptionalWithAccount::to_json(row).unwrap();
    }
    let nested = db
        .query::<Account>()
        .filter(Account::ID.eq(alice.id))
        .prefetch_related(Post::USER.reverse().prefetch(Comment::POST.reverse()))
        .all(&db)
        .await
        .unwrap();
    let json = AccountWithPosts::to_json(nested.into_iter().next().unwrap()).unwrap();
    assert_eq!(
        json["posts"][0]["comments"][0]["id"],
        comment.id.to_string()
    );
    let multi = db
        .query::<Account>()
        .filter(Account::ID.eq(alice.id))
        .prefetch_related(Post::USER.reverse())
        .prefetch_related(OptionalPost::USER.reverse())
        .all(&db)
        .await
        .unwrap();
    assert_eq!(multi[0].relations.0.related[0].id, post.id);
    assert_eq!(multi[0].relations.1.related[0].id, attached.id);
    let invalid = db
        .create::<Post>()
        .set(Post::USER_ID, Uuid::now_v7())
        .set(Post::EDITOR_ID, bob.id)
        .set(Post::TITLE, "Invalid")
        .execute()
        .await
        .unwrap_err();
    #[cfg(feature = "postgres")]
    assert_eq!(invalid.sql_state(), Some("23503"));
    assert_eq!(
        invalid.constraint_kind(),
        Some(che_orm::ConstraintKind::ForeignKey)
    );
    #[cfg(feature = "sqlite")]
    assert!(matches!(invalid, OrmError::Sqlite(_)));
    let rollback: Result<(), OrmError> = db
        .transaction(move |tx| {
            Box::pin(async move {
                tx.update::<Account>(alice.id)
                    .set(Account::NAME, "Rolled back")
                    .execute()
                    .await?;
                assert_eq!(
                    tx.get::<Account>(alice.id).await?.unwrap().name,
                    "Rolled back"
                );
                Err(OrmError::Interaction("rollback".into()))
            })
        })
        .await;
    assert!(rollback.is_err());
    assert_eq!(
        db.get::<Account>(alice.id).await.unwrap().unwrap().name,
        "Alice"
    );
    db.transaction(move |tx| {
        Box::pin(async move {
            tx.update::<Account>(bob.id)
                .set(Account::NAME, "Committed")
                .execute()
                .await?;
            assert_eq!(
                tx.query::<Account>()
                    .filter(Account::ID.eq(bob.id))
                    .first()
                    .await?
                    .unwrap()
                    .name,
                "Committed"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
    #[cfg(feature = "postgres")]
    {
        let caught_error = db
            .transaction(move |tx| {
                Box::pin(async move {
                    tx.update::<Account>(alice.id)
                        .set(Account::NAME, "Uncommitted")
                        .execute()
                        .await?;
                    let error = tx
                        .update::<Account>(bob.id)
                        .set(Account::NAME, "Uncommitted")
                        .execute()
                        .await
                        .unwrap_err();
                    assert_eq!(
                        error.constraint_kind(),
                        Some(che_orm::ConstraintKind::Unique)
                    );
                    Ok("must not succeed")
                })
            })
            .await
            .unwrap_err();
        assert_eq!(caught_error.sql_state(), Some("25P02"));
        assert_eq!(
            db.get::<Account>(alice.id).await.unwrap().unwrap().name,
            "Alice"
        );
        assert_eq!(
            db.get::<Account>(bob.id).await.unwrap().unwrap().name,
            "Committed"
        );
    }
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let signal = entered.clone();
    let db_clone = db.clone();
    let task = tokio::spawn(async move {
        db_clone
            .transaction(move |tx| {
                Box::pin(async move {
                    tx.update::<Account>(bob.id)
                        .set(Account::NAME, "Cancelled")
                        .execute()
                        .await?;
                    signal.notify_one();
                    std::future::pending::<Result<(), OrmError>>().await
                })
            })
            .await
    });
    entered.notified().await;
    task.abort();
    let _ = task.await;
    let restored =
        tokio::time::timeout(std::time::Duration::from_secs(5), db.get::<Account>(bob.id))
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    assert_eq!(restored.name, "Committed");
    assert!(db.delete::<Account>(alice.id).await.unwrap());
    assert!(db.get::<Post>(post.id).await.unwrap().is_none());
    assert!(db.get::<Comment>(comment.id).await.unwrap().is_none());
    assert!(
        db.get::<OptionalPost>(attached.id)
            .await
            .unwrap()
            .unwrap()
            .user_id
            .is_none()
    );
    migrations(&db).await;
    if let Some((admin, schema)) = cleanup {
        admin
            .execute_sql(format!("DROP SCHEMA \"{schema}\" CASCADE"), vec![])
            .await
            .unwrap();
    }
}

async fn migrations(db: &Database) {
    let desired = ProjectState::from_app_schema(
        "portable",
        &SchemaSet::new()
            .model::<MigrationParent>()
            .model::<MigrationChild>(),
    );
    let initial = Migration::new(
        MigrationId {
            app: "portable".into(),
            name: "0001_initial".into(),
        },
        vec![],
        desired
            .models()
            .into_iter()
            .map(|model| MigrationOperation::CreateModel { model })
            .collect(),
    );
    let graph = MigrationGraph::new(vec![initial.clone()]).unwrap();
    let (first, second) = tokio::join!(db.migrate(graph.clone()), db.migrate(graph.clone()));
    assert_eq!(first.unwrap() + second.unwrap(), 1);
    assert_eq!(db.migrate(graph).await.unwrap(), 0);
    let parent = db
        .create::<MigrationParent>()
        .set(MigrationParent::NAME, "Kept")
        .execute()
        .await
        .unwrap();
    let child = db
        .create::<MigrationChild>()
        .set(MigrationChild::PARENT_ID, parent.id)
        .execute()
        .await
        .unwrap();
    db.update::<MigrationParent>(parent.id)
        .set(MigrationParent::CHILD_ID, Some(child.id))
        .execute()
        .await
        .unwrap();
    let table = desired
        .tables()
        .iter()
        .find(|table| table.name == MigrationParent::table_name())
        .unwrap();
    let old = table
        .columns
        .iter()
        .find(|column| column.name == "name")
        .unwrap()
        .clone();
    let mut new = old.clone();
    new.nullable = true;
    new.unique = true;
    let change = Migration::new(
        MigrationId {
            app: "portable".into(),
            name: "0002_change".into(),
        },
        vec![initial.id.clone()],
        vec![
            MigrationOperation::AlterColumn {
                table: table.name.clone(),
                old_column: old,
                new_column: new,
            },
            MigrationOperation::AddIndex {
                table: table.name.clone(),
                columns: vec!["child_id".into()],
            },
            MigrationOperation::RemoveIndex {
                table: table.name.clone(),
                columns: vec!["name".into()],
            },
            MigrationOperation::RemoveIndex {
                table: table.name.clone(),
                columns: vec!["child_id".into()],
            },
            MigrationOperation::RenameColumn {
                table: table.name.clone(),
                old_name: "name".into(),
                new_name: "label".into(),
            },
            MigrationOperation::RenameTable {
                old_name: table.name.clone(),
                new_name: "portable_migration_renamed".into(),
            },
        ],
    );
    let graph = MigrationGraph::new(vec![initial.clone(), change.clone()]).unwrap();
    assert_eq!(
        db.migrate_to(graph.clone(), Some(initial.id.clone()))
            .await
            .unwrap(),
        0
    );
    assert_eq!(db.migrate(graph.clone()).await.unwrap(), 1);
    assert_eq!(db.migrate(graph).await.unwrap(), 0);
    let rows = db
        .query_sql(
            "SELECT \"id\", \"label\", \"child_id\" FROM \"portable_migration_renamed\"",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(rows[0].get::<Uuid>(0).unwrap(), parent.id);
    assert_eq!(rows[0].get::<String>(1).unwrap(), "Kept");
    assert_eq!(rows[0].get::<Uuid>(2).unwrap(), child.id);
    let mut duplicate = initial.clone();
    duplicate.checksum = "changed".into();
    assert!(
        db.migrate(MigrationGraph::new(vec![duplicate, change.clone()]).unwrap())
            .await
            .is_err()
    );
    let extra: che_orm::ColumnState =
        (&che_orm::ColumnSchema::new("extra", che_orm::ColumnType::Integer, true)).into();
    let mut altered = extra.clone();
    altered.column_type = che_orm::ColumnType::Real;
    altered.nullable = false;
    altered.default = Some("2.5".into());
    altered.check = Some("extra >= 0".into());
    let extra_migration=Migration::new(MigrationId {app:"portable".into(),name:"0003_extra".into()},vec![change.id.clone()],vec![
        MigrationOperation::AddColumn {table:"portable_migration_renamed".into(),column:extra.clone()},
        MigrationOperation::BackfillColumn {table:"portable_migration_renamed".into(),column:"extra".into(),value:"7".into()},
        MigrationOperation::AlterColumn {table:"portable_migration_renamed".into(),old_column:extra,new_column:altered},
        MigrationOperation::AddUniqueConstraint {table:"portable_migration_renamed".into(),columns:vec!["label".into(),"extra".into()]},
        MigrationOperation::CreateSchemaObject {object:che_orm::SchemaObjectState {app:"portable".into(),kind:che_orm::SchemaObjectKind::View,
            name:"portable_view".into(),sql:"CREATE VIEW portable_view AS SELECT label, extra FROM portable_migration_renamed".into()}},
    ]);
    assert_eq!(
        db.migrate(
            MigrationGraph::new(vec![
                initial.clone(),
                change.clone(),
                extra_migration.clone()
            ])
            .unwrap()
        )
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_sql("SELECT extra FROM portable_view", vec![])
            .await
            .unwrap()[0]
            .get::<f64>(0)
            .unwrap(),
        7.0
    );
    let cleanup = Migration::new(
        MigrationId {
            app: "portable".into(),
            name: "0004_cleanup".into(),
        },
        vec![extra_migration.id.clone()],
        vec![
            MigrationOperation::DropSchemaObject {
                kind: che_orm::SchemaObjectKind::View,
                name: "portable_view".into(),
            },
            MigrationOperation::RemoveUniqueConstraint {
                table: "portable_migration_renamed".into(),
                columns: vec!["label".into(), "extra".into()],
            },
            MigrationOperation::RemoveColumn {
                table: "portable_migration_renamed".into(),
                column: "extra".into(),
            },
        ],
    );
    let mut completed = vec![initial, change, extra_migration, cleanup.clone()];
    assert_eq!(
        db.migrate(MigrationGraph::new(completed.clone()).unwrap())
            .await
            .unwrap(),
        1
    );
    #[cfg(feature = "postgres")]
    {
        let trigger=Migration::new(MigrationId {app:"portable".into(),name:"0005_trigger".into()},vec![cleanup.id.clone()],vec![
            MigrationOperation::RunSql {forward:"CREATE FUNCTION portable_guard() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.label = 'blocked' THEN RAISE EXCEPTION 'blocked'; END IF; RETURN NEW; END $$".into(),reverse:None,state_operations:vec![]},
            MigrationOperation::CreateSchemaObject {object:che_orm::SchemaObjectState {app:"portable".into(),kind:che_orm::SchemaObjectKind::Trigger,
                name:"portable_guard".into(),sql:"CREATE TRIGGER portable_guard BEFORE INSERT ON portable_migration_renamed FOR EACH ROW EXECUTE FUNCTION portable_guard()".into()}},
        ]);
        completed.push(trigger.clone());
        assert_eq!(
            db.migrate(MigrationGraph::new(completed.clone()).unwrap())
                .await
                .unwrap(),
            1
        );
        let insert = che_orm::QueryAst::Insert(che_orm::InsertAst {
            table: che_orm::TableRef::new("portable_migration_renamed"),
            returning: vec![],
            values: vec![
                che_orm::InsertValue {
                    column: che_orm::ColumnRef::new("portable_migration_renamed", "id"),
                    value: che_orm::DatabaseValue::Uuid(Uuid::now_v7()),
                },
                che_orm::InsertValue {
                    column: che_orm::ColumnRef::new("portable_migration_renamed", "label"),
                    value: che_orm::DatabaseValue::Text("blocked".into()),
                },
            ],
        });
        assert_eq!(
            db.execute(insert).await.unwrap_err().sql_state(),
            Some("P0001")
        );
        completed.push(Migration::new(
            MigrationId {
                app: "portable".into(),
                name: "0006_drop_trigger".into(),
            },
            vec![trigger.id],
            vec![
                MigrationOperation::DropSchemaObject {
                    kind: che_orm::SchemaObjectKind::Trigger,
                    name: "portable_guard".into(),
                },
                MigrationOperation::RunSql {
                    forward: "DROP FUNCTION portable_guard()".into(),
                    reverse: None,
                    state_operations: vec![],
                },
            ],
        ));
        assert_eq!(
            db.migrate(MigrationGraph::new(completed.clone()).unwrap())
                .await
                .unwrap(),
            1
        );
    }
    let expected_history = completed.len();
    let failure = Migration::new(
        MigrationId {
            app: "portable".into(),
            name: "0007_failure".into(),
        },
        vec![completed.last().unwrap().id.clone()],
        vec![
            MigrationOperation::RunSql {
                forward: "CREATE TABLE portable_rollback (id BIGINT)".into(),
                reverse: None,
                state_operations: vec![],
            },
            MigrationOperation::RunSql {
                forward: "SELECT '\\'; COMMIT; SELECT 'abc'; SELECT 1/0".into(),
                reverse: None,
                state_operations: vec![],
            },
        ],
    );
    completed.push(failure);
    assert!(
        db.migrate(MigrationGraph::new(completed).unwrap())
            .await
            .is_err()
    );
    assert_eq!(
        db.migration_history().await.unwrap().len(),
        expected_history
    );
    assert!(
        db.query_sql("SELECT * FROM portable_rollback", vec![])
            .await
            .is_err()
    );
    assert_eq!(
        db.query_sql("SELECT COUNT(*) FROM portable_migration_renamed", vec![])
            .await
            .unwrap()[0]
            .get::<i64>(0)
            .unwrap(),
        1
    );
}

#[cfg(feature = "postgres")]
fn migration_table(name: &str, columns: Vec<che_orm::ColumnState>) -> che_orm::TableState {
    che_orm::TableState {
        name: name.into(),
        columns,
        unique_constraints: vec![],
        indexes: vec![],
    }
}

#[cfg(feature = "postgres")]
fn integer_key() -> che_orm::ColumnState {
    let mut column = che_orm::ColumnSchema::new("id", che_orm::ColumnType::Integer, false);
    column.primary_key = true;
    (&column).into()
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL and a live PostgreSQL server"]
async fn alter_column_removes_old_type_dependencies_before_conversion() {
    use che_orm::{
        ColumnSchema, ColumnState, ColumnType, ConstraintKind, DatabaseValue, ForeignKeyState,
    };
    let (db, cleanup) = fixture().await;
    let mut old: ColumnState = (&ColumnSchema::new("parent_id", ColumnType::Integer, false)).into();
    old.references = Some(ForeignKeyState {
        target: "alter_parent(id)".into(),
        on_delete: None,
    });
    old.check = Some("parent_id > 0".into());
    old.choices = Some(vec!["1".into(), "2".into()]);
    old.default = Some("1".into());
    let initial = Migration::new(
        MigrationId {
            app: "alter".into(),
            name: "0001".into(),
        },
        vec![],
        vec![
            MigrationOperation::CreateTable {
                table: migration_table("alter_parent", vec![integer_key()]),
            },
            MigrationOperation::CreateTable {
                table: migration_table("alter_child", vec![integer_key(), old.clone()]),
            },
        ],
    );
    db.migrate(MigrationGraph::new(vec![initial.clone()]).unwrap())
        .await
        .unwrap();
    db.execute_sql("INSERT INTO alter_parent DEFAULT VALUES", vec![])
        .await
        .unwrap();
    db.execute_sql("INSERT INTO alter_child DEFAULT VALUES", vec![])
        .await
        .unwrap();
    let mut new = old.clone();
    new.column_type = ColumnType::Text;
    new.references = None;
    new.check = Some("length(parent_id) > 0".into());
    new.choices = Some(vec!["1".into(), "kept".into()]);
    new.default = Some("'kept'".into());
    let change = Migration::new(
        MigrationId {
            app: "alter".into(),
            name: "0002".into(),
        },
        vec![initial.id.clone()],
        vec![MigrationOperation::AlterColumn {
            table: "alter_child".into(),
            old_column: old,
            new_column: new,
        }],
    );
    let graph = MigrationGraph::new(vec![initial, change]).unwrap();
    assert_eq!(db.migrate(graph.clone()).await.unwrap(), 1);
    assert_eq!(db.migrate(graph).await.unwrap(), 0);
    let rows = db
        .query_sql("SELECT parent_id FROM alter_child", vec![])
        .await
        .unwrap();
    assert_eq!(rows[0].get::<String>(0).unwrap(), "1");
    db.execute_sql("INSERT INTO alter_child DEFAULT VALUES", vec![])
        .await
        .unwrap();
    let rows = db
        .query_sql("SELECT parent_id FROM alter_child ORDER BY id", vec![])
        .await
        .unwrap();
    assert_eq!(rows[1].get::<String>(0).unwrap(), "kept");
    let invalid = db
        .execute_sql(
            "INSERT INTO alter_child (parent_id) VALUES ($1)",
            vec![DatabaseValue::Text("invalid".into())],
        )
        .await
        .unwrap_err();
    assert_eq!(invalid.constraint_kind(), Some(ConstraintKind::Check));
    if let Some((admin, schema)) = cleanup {
        admin
            .execute_sql(format!("DROP SCHEMA \"{schema}\" CASCADE"), vec![])
            .await
            .unwrap();
    }
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL and a live PostgreSQL server"]
async fn deferred_foreign_keys_follow_schema_operations() {
    use che_orm::{
        ColumnSchema, ColumnState, ColumnType, ConstraintKind, DatabaseValue, ForeignKeyState,
    };
    for case in [
        "ready_before_rename",
        "rename_and_replace",
        "drop_column",
        "drop_table",
        "wait_for_unique",
    ] {
        let (db, cleanup) = fixture().await;
        let mut fk: ColumnState =
            (&ColumnSchema::new("parent_id", ColumnType::Integer, true)).into();
        fk.references = Some(ForeignKeyState {
            target: "deferred_parent(id)".into(),
            on_delete: None,
        });
        let child = migration_table("deferred_child", vec![integer_key(), fk.clone()]);
        let parent = migration_table("deferred_parent", vec![integer_key()]);
        let mut operations = vec![MigrationOperation::CreateTable { table: child }];
        let mut source = "deferred_child";
        let mut column = "parent_id";
        let mut target = "deferred_parent";
        match case {
            "ready_before_rename" => {
                operations.push(MigrationOperation::CreateTable { table: parent });
                operations.push(MigrationOperation::RenameTable {
                    old_name: source.into(),
                    new_name: "renamed_child".into(),
                });
                source = "renamed_child";
            }
            "rename_and_replace" => {
                operations.push(MigrationOperation::RenameTable {
                    old_name: source.into(),
                    new_name: "renamed_child".into(),
                });
                source = "renamed_child";
                operations.push(MigrationOperation::RenameColumn {
                    table: source.into(),
                    old_name: column.into(),
                    new_name: "owner_id".into(),
                });
                column = "owner_id";
                fk.name = column.into();
                let mut replacement = fk.clone();
                replacement.references = Some(ForeignKeyState {
                    target: "replacement_parent(id)".into(),
                    on_delete: Some("cascade".into()),
                });
                operations.push(MigrationOperation::AlterColumn {
                    table: source.into(),
                    old_column: fk,
                    new_column: replacement,
                });
                target = "replacement_parent";
                operations.push(MigrationOperation::CreateTable {
                    table: migration_table(target, vec![integer_key()]),
                });
            }
            "drop_column" => {
                operations.push(MigrationOperation::RemoveColumn {
                    table: source.into(),
                    column: column.into(),
                });
                operations.push(MigrationOperation::CreateTable { table: parent });
            }
            "drop_table" => {
                operations.push(MigrationOperation::DropTable {
                    name: source.into(),
                });
                operations.push(MigrationOperation::CreateTable { table: parent });
            }
            "wait_for_unique" => {
                let key: ColumnState =
                    (&ColumnSchema::new("id", ColumnType::Integer, false)).into();
                operations.push(MigrationOperation::CreateTable {
                    table: migration_table(target, vec![key]),
                });
                operations.push(MigrationOperation::RenameTable {
                    old_name: source.into(),
                    new_name: "renamed_child".into(),
                });
                source = "renamed_child";
                operations.push(MigrationOperation::AddUniqueConstraint {
                    table: target.into(),
                    columns: vec!["id".into()],
                });
            }
            _ => unreachable!(),
        }
        let initial = Migration::new(
            MigrationId {
                app: "deferred".into(),
                name: "0001".into(),
            },
            vec![],
            operations,
        );
        let graph = MigrationGraph::new(vec![initial]).unwrap();
        assert_eq!(db.migrate(graph.clone()).await.unwrap(), 1, "{case}");
        assert_eq!(db.migrate(graph).await.unwrap(), 0, "{case}");
        let count = db.query_sql("SELECT COUNT(*) FROM pg_constraint WHERE contype = 'f' AND conrelid = to_regclass($1)", vec![DatabaseValue::Text(source.into())]).await.unwrap()[0].get::<i64>(0).unwrap();
        if matches!(case, "drop_column" | "drop_table") {
            assert_eq!(count, 0, "{case}");
        } else {
            assert_eq!(count, 1, "{case}");
            db.execute_sql(format!("INSERT INTO \"{target}\" (id) VALUES (1)"), vec![])
                .await
                .unwrap();
            db.execute_sql(
                format!("INSERT INTO \"{source}\" (\"{column}\") VALUES ($1)"),
                vec![DatabaseValue::Integer(1)],
            )
            .await
            .unwrap();
            let error = db
                .execute_sql(
                    format!("INSERT INTO \"{source}\" (\"{column}\") VALUES ($1)"),
                    vec![DatabaseValue::Integer(999)],
                )
                .await
                .unwrap_err();
            assert_eq!(
                error.constraint_kind(),
                Some(ConstraintKind::ForeignKey),
                "{case}"
            );
            if case == "rename_and_replace" {
                db.execute_sql(format!("DELETE FROM \"{target}\" WHERE id = 1"), vec![])
                    .await
                    .unwrap();
                assert_eq!(
                    db.query_sql(format!("SELECT COUNT(*) FROM \"{source}\""), vec![])
                        .await
                        .unwrap()[0]
                        .get::<i64>(0)
                        .unwrap(),
                    0
                );
            }
        }
        if let Some((admin, schema)) = cleanup {
            admin
                .execute_sql(format!("DROP SCHEMA \"{schema}\" CASCADE"), vec![])
                .await
                .unwrap();
        }
    }
}
