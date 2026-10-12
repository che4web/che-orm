use che_orm::{
    Database, Migration, MigrationGraph, MigrationId, MigrationOperation, Model, ModelRole,
    OrmError, ProjectState, SchemaSet, SyncModel, Uuid, ValidatedWrite,
};
use che_sync::{
    AllowAll, LoadRequired, Registry, SyncClient, SyncEngine, SyncServer, SyncTransport,
    TransportFuture, protocol::*,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Debug, Model)]
#[orm(table = "role_tasks", sync, sync_name = "tasks.task")]
struct Task {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(db_column = "task_title", sync_name = "title")]
    title: String,
    #[orm(default = "''")]
    body: String,
    #[orm(default = "false")]
    done: bool,
}

#[derive(Debug, Model)]
#[orm(table = "role_children", sync, sync_name = "tasks.child")]
struct Child {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(foreign_key = Task, on_delete = "cascade")]
    task_id: Uuid,
    label: String,
}

#[derive(Debug, Model)]
#[orm(table = "role_settings")]
struct Setting {
    #[orm(primary_key)]
    id: i64,
    value: String,
}

fn schema(role: ModelRole) -> SchemaSet {
    let schema = SchemaSet::new().model::<Setting>();
    match role {
        ModelRole::Client => schema.client_model::<Task>().client_model::<Child>(),
        ModelRole::Server => schema.server_model::<Task>().server_model::<Child>(),
    }
}

fn state(schema: &SchemaSet) -> ProjectState {
    ProjectState::from_app_schemas([("tasks", schema)]).unwrap()
}

#[test]
fn roles_generate_only_their_own_infrastructure() {
    let client = schema(ModelRole::Client);
    let server = schema(ModelRole::Server);
    assert_eq!(Task::schema().columns.len(), 4);
    assert_eq!(Task::sync_schema().columns.len(), 8);
    let sql = che_orm::SqlCompiler::<che_orm::SqliteDialect>::compile_schema(&Task::sync_schema());
    assert!(sql.table.contains("task_title TEXT NOT NULL"));
    assert!(sql.table.contains("revision INTEGER NOT NULL DEFAULT 0"));
    assert!(
        Task::registered_fields()
            .iter()
            .any(|field| field.name == "title" && field.column == "task_title" && field.writable)
    );
    assert!(
        Task::registered_fields()
            .iter()
            .any(|field| field.name == "id" && !field.writable)
    );
    assert_eq!(client.sync_models()[0].role, ModelRole::Client);
    assert_eq!(server.sync_models()[0].role, ModelRole::Server);
    let client_state = state(&client);
    let server_state = state(&server);
    assert!(
        client_state
            .tables()
            .iter()
            .any(|table| table.name == "_sync_server_role_tasks")
    );
    assert!(
        client_state
            .tables()
            .iter()
            .any(|table| table.name == "_sync_outbox")
    );
    assert!(
        !client_state
            .tables()
            .iter()
            .any(|table| table.name == "_sync_changes")
    );
    assert!(
        server_state
            .tables()
            .iter()
            .any(|table| table.name == "_sync_changes")
    );
    assert!(
        !server_state
            .tables()
            .iter()
            .any(|table| table.name.starts_with("_sync_server_"))
    );
    assert!(
        !server_state
            .tables()
            .iter()
            .any(|table| table.name == "_sync_outbox")
    );
    assert_eq!(
        Registry::from_schema(&client).unwrap().role(),
        Some(ModelRole::Client)
    );
    let mut desired = server_state.clone();
    Registry::from_schema(&server)
        .unwrap()
        .extend_migration_state(&mut desired, ModelRole::Server)
        .unwrap();
    assert_eq!(desired, server_state);
    assert!(
        Registry::from_schema(
            &SchemaSet::new()
                .client_model::<Task>()
                .server_model::<Child>()
        )
        .is_err()
    );
    assert!(
        Registry::from_schema(
            &SchemaSet::new()
                .client_model::<Task>()
                .server_model::<Task>()
        )
        .is_err()
    );
}

struct DenyChildDeletion;

struct RevocablePolicy(Arc<AtomicBool>);

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL"]
async fn execute_select_enforces_scope_tombstones_and_revocation() {
    let schema = schema(ModelRole::Server);
    let (db, cleanup) = database(&schema).await;
    let allowed = Arc::new(AtomicBool::new(true));
    let server = SyncServer::from_schema(
        db.clone(),
        &schema,
        "first",
        "principal",
        Arc::new(RevocablePolicy(allowed.clone())),
    )
    .await
    .unwrap();
    let other = SyncServer::from_schema(db, &schema, "second", "principal", Arc::new(AllowAll))
        .await
        .unwrap();
    server
        .database()
        .create::<Task>()
        .set(Task::TITLE, "private")
        .execute()
        .await
        .unwrap();
    let deleted = server
        .database()
        .create::<Task>()
        .set(Task::TITLE, "deleted")
        .execute()
        .await
        .unwrap();
    server.database().delete::<Task>(deleted.id).await.unwrap();
    let select = || Task::query().into_ast().unwrap();
    assert_eq!(
        server
            .database()
            .execute(select())
            .await
            .unwrap()
            .rows_affected,
        1
    );
    assert_eq!(
        other
            .database()
            .execute(select())
            .await
            .unwrap()
            .rows_affected,
        0
    );
    assert_eq!(
        other
            .database()
            .execute(
                Task::query()
                    .filter(Task::TITLE.eq("private"))
                    .into_ast()
                    .unwrap()
            )
            .await
            .unwrap()
            .rows_affected,
        0
    );
    other
        .database()
        .transaction(|tx| {
            Box::pin(async move {
                assert_eq!(
                    tx.execute(Task::query().into_ast()?).await?.rows_affected,
                    0
                );
                Ok(())
            })
        })
        .await
        .unwrap();
    let policy = allowed.clone();
    server.database().transaction(move |tx| Box::pin(async move {
        assert_eq!(tx.execute(Task::query().into_ast()?).await?.rows_affected, 1);
        policy.store(false, Ordering::SeqCst);
        assert!(matches!(tx.execute(Task::query().into_ast()?).await, Err(OrmError::Configuration(message)) if message == "scope access revoked"));
        Ok(())
    })).await.unwrap();
    assert!(
        matches!(server.database().execute(select()).await, Err(OrmError::Configuration(message)) if message == "scope access revoked")
    );
    if let Some((admin, name)) = cleanup {
        admin
            .execute_sql(format!("DROP SCHEMA \"{name}\" CASCADE"), vec![])
            .await
            .unwrap();
    }
}

impl che_sync::AccessPolicy for RevocablePolicy {
    fn authorize_scope(&self, _: &str, _: &str) -> Result<(), OrmError> {
        if self.0.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(OrmError::Configuration("scope access revoked".into()))
        }
    }
    fn validate(&self, _: &str, _: &str, _: &Entity, _: &Entity) -> Result<(), OrmError> {
        Ok(())
    }
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn revoked_scope_blocks_existing_orm_handles_and_transactions() {
    let schema = schema(ModelRole::Server);
    let (db, cleanup) = database(&schema).await;
    let allowed = Arc::new(AtomicBool::new(true));
    let server = SyncServer::from_schema(
        db,
        &schema,
        "scope",
        "principal",
        Arc::new(RevocablePolicy(allowed.clone())),
    )
    .await
    .unwrap();
    let task = server
        .database()
        .create::<Task>()
        .set(Task::TITLE, "private")
        .execute()
        .await
        .unwrap();
    let handle = server.database().clone();
    allowed.store(false, Ordering::SeqCst);
    assert!(handle.get::<Task>(task.id).await.is_err());
    assert!(handle.all::<Task>().await.is_err());
    assert!(handle.count::<Task>().await.is_err());
    assert!(
        handle
            .fetch_all(Task::query().filter(Task::TITLE.eq("private")))
            .await
            .is_err()
    );
    assert!(
        handle
            .query::<Child>()
            .select_related(Child::TASK)
            .all(&handle)
            .await
            .is_err()
    );
    let allowed_in_transaction = allowed.clone();
    allowed.store(true, Ordering::SeqCst);
    handle
        .transaction(move |tx| {
            Box::pin(async move {
                assert_eq!(tx.count::<Task>().await?, 1);
                allowed_in_transaction.store(false, Ordering::SeqCst);
                assert!(tx.get::<Task>(task.id).await.is_err());
                assert!(tx.count::<Task>().await.is_err());
                Ok(())
            })
        })
        .await
        .unwrap();
    // Local, non-sync models are not governed by this scope policy.
    assert_eq!(handle.count::<Setting>().await.unwrap(), 0);
    if let Some((admin, name)) = cleanup {
        admin
            .execute_sql(format!("DROP SCHEMA \"{name}\" CASCADE"), vec![])
            .await
            .unwrap();
    }
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn expired_cursor_blocks_new_writes_until_explicit_full_load() {
    let server_schema = schema(ModelRole::Server);
    let client_schema = schema(ModelRole::Client);
    let (server_db, server_cleanup) = database(&server_schema).await;
    let (client_db, client_cleanup) = database(&client_schema).await;
    let server = SyncServer::from_schema(
        server_db.clone(),
        &server_schema,
        "scope",
        "principal",
        Arc::new(AllowAll),
    )
    .await
    .unwrap();
    let task = server
        .database()
        .create::<Task>()
        .set(Task::TITLE, "base")
        .execute()
        .await
        .unwrap();
    let client = SyncClient::from_schema(client_db.clone(), &client_schema, "scope")
        .await
        .unwrap();
    let transport = RestLikeTransport::new(server.for_model::<Task>().unwrap());
    let feed = client.model::<Task, _>(transport.clone()).unwrap();
    feed.full_load().await.unwrap();
    client
        .database()
        .update::<Task>(task.id)
        .set(Task::TITLE, "offline")
        .execute()
        .await
        .unwrap();
    server
        .database()
        .update::<Task>(task.id)
        .set(Task::BODY, "new server body")
        .execute()
        .await
        .unwrap();
    server_db
        .execute_sql("DELETE FROM _sync_changes", vec![])
        .await
        .unwrap();
    assert_eq!(
        feed.pull().await.unwrap().load_required,
        Some(LoadRequired::Reset)
    );
    assert_eq!(
        feed.flush().await.unwrap().load_required,
        Some(LoadRequired::Reset)
    );
    assert_eq!(transport.pushes.load(Ordering::SeqCst), 0);
    drop(feed);
    drop(client);
    let reopened = SyncClient::from_schema(client_db, &client_schema, "scope")
        .await
        .unwrap();
    let feed = reopened.model::<Task, _>(transport.clone()).unwrap();
    assert_eq!(
        feed.flush().await.unwrap().load_required,
        Some(LoadRequired::Reset)
    );
    assert_eq!(transport.pushes.load(Ordering::SeqCst), 0);
    feed.full_load().await.unwrap();
    assert_eq!(
        reopened
            .database()
            .get::<Task>(task.id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "offline"
    );
    assert_eq!(feed.flush().await.unwrap().accepted_operations, 1);
    for (admin, name) in [server_cleanup, client_cleanup].into_iter().flatten() {
        admin
            .execute_sql(format!("DROP SCHEMA \"{name}\" CASCADE"), vec![])
            .await
            .unwrap();
    }
}

#[test]
fn sql_export_and_migration_state_have_identical_tables() {
    fn check<D: che_orm::SqlDialect>(schema: &SchemaSet) {
        let sql = schema.to_sql::<D>();
        let desired = state(schema);
        assert_eq!(sql.matches("CREATE TABLE ").count(), desired.tables().len());
        for table in desired.tables() {
            let compiled = che_orm::SqlCompiler::<D>::compile_table_state(table);
            assert_eq!(
                sql.matches(&compiled.table).count(),
                1,
                "{} missing or duplicated",
                table.name
            );
        }
    }
    for role in [ModelRole::Client, ModelRole::Server] {
        let schema = match role {
            ModelRole::Client => SchemaSet::new()
                .client_model::<Task>()
                .merge(SchemaSet::new().client_model::<Child>()),
            ModelRole::Server => SchemaSet::new()
                .server_model::<Task>()
                .merge(SchemaSet::new().server_model::<Child>()),
        };
        check::<che_orm::SqliteDialect>(&schema);
        check::<che_orm::PostgresDialect>(&schema);
    }
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn expired_cursor_preserves_and_retries_unknown_operation() {
    let server_schema = schema(ModelRole::Server);
    let client_schema = schema(ModelRole::Client);
    let (server_db, server_cleanup) = database(&server_schema).await;
    let (client_db, client_cleanup) = database(&client_schema).await;
    let server = SyncServer::from_schema(
        server_db.clone(),
        &server_schema,
        "scope",
        "principal",
        Arc::new(AllowAll),
    )
    .await
    .unwrap();
    let client = SyncClient::from_schema(client_db.clone(), &client_schema, "scope")
        .await
        .unwrap();
    let transport = RestLikeTransport::new(server.for_model::<Task>().unwrap());
    let feed = client.model::<Task, _>(transport.clone()).unwrap();
    feed.full_load().await.unwrap();
    let task = client
        .database()
        .create::<Task>()
        .set(Task::TITLE, "sent")
        .execute()
        .await
        .unwrap();
    transport.lose_reply.store(true, Ordering::SeqCst);
    assert!(feed.flush().await.is_err());
    let outbox = client_db
        .query_sql("SELECT payload FROM _sync_outbox", vec![])
        .await
        .unwrap()[0]
        .get::<String>(0)
        .unwrap();
    client
        .database()
        .update::<Task>(task.id)
        .set(Task::TITLE, "edited after send")
        .execute()
        .await
        .unwrap();
    server_db
        .execute_sql("DELETE FROM _sync_changes", vec![])
        .await
        .unwrap();
    assert_eq!(
        feed.pull().await.unwrap().load_required,
        Some(LoadRequired::Reset)
    );
    assert_eq!(transport.pushes.load(Ordering::SeqCst), 1);
    let blocked = feed.full_load().await.unwrap();
    assert!(blocked.blocked);
    assert_eq!(blocked.load_required, Some(LoadRequired::Reset));
    assert_eq!(
        client_db
            .query_sql("SELECT payload FROM _sync_outbox", vec![])
            .await
            .unwrap()[0]
            .get::<String>(0)
            .unwrap(),
        outbox
    );
    // Retry the original identity to obtain its receipt. Do not send a fresh
    // operation for the edit made after the lost reply until full loading.
    let retried = feed.flush().await.unwrap();
    assert_eq!(retried.accepted_operations, 1);
    assert_eq!(retried.load_required, Some(LoadRequired::Reset));
    assert_eq!(transport.pushes.load(Ordering::SeqCst), 2);
    assert_eq!(
        feed.flush().await.unwrap().load_required,
        Some(LoadRequired::Reset)
    );
    assert_eq!(transport.pushes.load(Ordering::SeqCst), 2);
    assert_eq!(
        server
            .database()
            .get::<Task>(task.id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "sent"
    );
    assert_eq!(
        server_db
            .query_sql("SELECT operation_id FROM _sync_receipts", vec![])
            .await
            .unwrap()
            .len(),
        1
    );
    let loaded = feed.full_load().await.unwrap();
    assert!(!loaded.blocked);
    assert_eq!(loaded.conflicts, 0);
    assert_eq!(feed.flush().await.unwrap().accepted_operations, 1);
    assert_eq!(server.database().count::<Task>().await.unwrap(), 1);
    assert_eq!(
        server
            .database()
            .get::<Task>(task.id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "edited after send"
    );
    for (admin, name) in [server_cleanup, client_cleanup].into_iter().flatten() {
        admin
            .execute_sql(format!("DROP SCHEMA \"{name}\" CASCADE"), vec![])
            .await
            .unwrap();
    }
}
impl che_sync::AccessPolicy for DenyChildDeletion {
    fn authorize_scope(&self, _: &str, _: &str) -> Result<(), OrmError> {
        Ok(())
    }
    fn validate(&self, _: &str, _: &str, _: &Entity, next: &Entity) -> Result<(), OrmError> {
        if next.model == Child::SYNC_NAME && next.record.is_none() {
            return Err(OrmError::Configuration("child deletion denied".into()));
        }
        Ok(())
    }
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn explicit_orm_delete_tracks_a_known_row_without_a_visible_projection() {
    let server_schema = schema(ModelRole::Server);
    let client_schema = schema(ModelRole::Client);
    let (server_db, server_cleanup) = database(&server_schema).await;
    let (client_db, client_cleanup) = database(&client_schema).await;
    let server = SyncServer::from_schema(
        server_db,
        &server_schema,
        "scope",
        "principal",
        Arc::new(AllowAll),
    )
    .await
    .unwrap();
    let task = server
        .database()
        .create::<Task>()
        .set(Task::TITLE, "server row")
        .execute()
        .await
        .unwrap();
    let client = SyncClient::from_schema(client_db.clone(), &client_schema, "scope")
        .await
        .unwrap();
    let feed = client
        .model::<Task, _>(server.for_model::<Task>().unwrap())
        .unwrap();
    feed.full_load().await.unwrap();
    // Fixture representing a deferred current-table projection with a live shadow.
    client_db
        .execute_sql(
            "DELETE FROM role_tasks WHERE id = $1",
            vec![che_orm::DatabaseValue::Uuid(task.id)],
        )
        .await
        .unwrap();
    assert!(client.database().delete::<Task>(task.id).await.unwrap());
    assert_eq!(feed.flush().await.unwrap().accepted_operations, 1);
    assert!(
        server
            .database()
            .get::<Task>(task.id)
            .await
            .unwrap()
            .is_none()
    );
    for (admin, name) in [server_cleanup, client_cleanup].into_iter().flatten() {
        admin
            .execute_sql(format!("DROP SCHEMA \"{name}\" CASCADE"), vec![])
            .await
            .unwrap();
    }
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn bulk_writes_and_failed_cascade_are_transactional() {
    let schema = schema(ModelRole::Server);
    let (db, cleanup) = database(&schema).await;
    let server = SyncServer::from_schema(
        db.clone(),
        &schema,
        "scope",
        "principal",
        Arc::new(AllowAll),
    )
    .await
    .unwrap();
    let first = server
        .database()
        .create::<Task>()
        .set(Task::TITLE, "first")
        .execute()
        .await
        .unwrap();
    server
        .database()
        .create::<Task>()
        .set(Task::TITLE, "second")
        .execute()
        .await
        .unwrap();
    let child = server
        .database()
        .create::<Child>()
        .set(Child::TASK_ID, first.id)
        .set(Child::LABEL, "protected")
        .execute()
        .await
        .unwrap();
    let result = server
        .database()
        .execute(
            Task::update()
                .filter(Task::DONE.eq(false))
                .set(Task::DONE, true)
                .into_ast()
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result.rows_affected, 2);
    let group: ChangeGroup = serde_json::from_str(
        &db.query_sql(
            "SELECT payload FROM _sync_changes ORDER BY sequence DESC LIMIT 1",
            vec![],
        )
        .await
        .unwrap()[0]
            .get::<String>(0)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(group.entities.len(), 2);
    assert!(group.entities.iter().all(|entity| entity.revision == 2));
    let denied = SyncServer::from_schema(
        db.clone(),
        &schema,
        "scope",
        "principal",
        Arc::new(DenyChildDeletion),
    )
    .await
    .unwrap();
    let count = db
        .query_sql("SELECT sequence FROM _sync_changes", vec![])
        .await
        .unwrap()
        .len();
    let id = first.id;
    // Catching a write error must not commit its partially applied cascade.
    denied
        .database()
        .transaction(move |tx| {
            Box::pin(async move {
                assert!(tx.delete::<Task>(id).await.is_err());
                tx.create::<Setting>()
                    .set(Setting::VALUE, "transaction continues")
                    .execute()
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
    assert!(server.database().get::<Task>(id).await.unwrap().is_some());
    assert!(
        server
            .database()
            .get::<Child>(child.id)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(server.database().count::<Setting>().await.unwrap(), 1);
    assert_eq!(
        db.query_sql("SELECT sequence FROM _sync_changes", vec![])
            .await
            .unwrap()
            .len(),
        count
    );
    // A different scope cannot observe or mutate these rows through its ORM.
    let other = SyncServer::from_schema(
        db.clone(),
        &schema,
        "other",
        "principal",
        Arc::new(AllowAll),
    )
    .await
    .unwrap();
    assert_eq!(other.database().count::<Task>().await.unwrap(), 0);
    assert!(
        other
            .database()
            .update::<Task>(id)
            .set(Task::TITLE, "wrong scope")
            .execute()
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        server
            .database()
            .get::<Task>(id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "first"
    );
    if let Some((admin, name)) = cleanup {
        admin
            .execute_sql(format!("DROP SCHEMA \"{name}\" CASCADE"), vec![])
            .await
            .unwrap();
    }
}

#[derive(Clone)]
struct RestLikeTransport {
    server: SyncServer,
    pushes: Arc<AtomicUsize>,
    pulls: Arc<AtomicUsize>,
    snapshots: Arc<AtomicUsize>,
    lose_reply: Arc<AtomicBool>,
}

impl RestLikeTransport {
    fn new(server: SyncServer) -> Self {
        Self {
            server,
            pushes: Arc::new(AtomicUsize::new(0)),
            pulls: Arc::new(AtomicUsize::new(0)),
            snapshots: Arc::new(AtomicUsize::new(0)),
            lose_reply: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl SyncTransport for RestLikeTransport {
    fn max_mutations(&self) -> usize {
        1
    }
    fn push(&self, request: PushRequest) -> TransportFuture<'_, PushResult> {
        Box::pin(async move {
            self.pushes.fetch_add(1, Ordering::SeqCst);
            let result = self.server.push(request).await?;
            if self.lose_reply.swap(false, Ordering::SeqCst) {
                return Err(OrmError::Interaction("lost REST reply".into()));
            }
            Ok(result)
        })
    }
    fn push_model(&self, model: &str, request: PushRequest) -> TransportFuture<'_, PushResult> {
        let transport = Self {
            server: self.server.for_model_name(model).unwrap(),
            ..self.clone()
        };
        Box::pin(async move { transport.push(request).await })
    }
    fn pull(&self, request: PullRequest) -> TransportFuture<'_, PullResult> {
        self.pulls.fetch_add(1, Ordering::SeqCst);
        Box::pin(self.server.pull(request))
    }
    fn snapshot(&self) -> TransportFuture<'_, Snapshot> {
        self.snapshots.fetch_add(1, Ordering::SeqCst);
        Box::pin(self.server.snapshot())
    }
}

async fn database(schema: &SchemaSet) -> (Database, Option<(Database, String)>) {
    #[cfg(feature = "sqlite")]
    let (db, cleanup) = (Database::connect_in_memory().await.unwrap(), None);
    #[cfg(feature = "postgres")]
    let (db, cleanup) = {
        let url = std::env::var("TEST_POSTGRES_URL")
            .expect("set TEST_POSTGRES_URL to run PostgreSQL model role tests");
        let admin = Database::connect(url.clone()).await.unwrap();
        let name = format!("model_roles_{}", Uuid::now_v7().simple());
        admin
            .execute_sql(format!("CREATE SCHEMA \"{name}\""), vec![])
            .await
            .unwrap();
        let separator = if url.contains('?') { '&' } else { '?' };
        let db = Database::connect(format!("{url}{separator}options=-csearch_path%3D{name}"))
            .await
            .unwrap();
        (db, Some((admin, name)))
    };
    let operations = state(schema)
        .models()
        .into_iter()
        .map(|model| MigrationOperation::CreateModel { model })
        .collect();
    db.migrate(
        MigrationGraph::new(vec![Migration::new(
            MigrationId {
                app: "roles".into(),
                name: "0001_initial".into(),
            },
            vec![],
            operations,
        )])
        .unwrap(),
    )
    .await
    .unwrap();
    (db, cleanup)
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn ordinary_orm_writes_and_independent_pull_flush() {
    let server_schema = schema(ModelRole::Server);
    let client_schema = schema(ModelRole::Client);
    let (server_db, server_cleanup) = database(&server_schema).await;
    let (a_db, a_cleanup) = database(&client_schema).await;
    let (b_db, b_cleanup) = database(&client_schema).await;
    let server = SyncServer::from_schema(
        server_db.clone(),
        &server_schema,
        "scope",
        "principal",
        Arc::new(AllowAll),
    )
    .await
    .unwrap();
    let a = SyncClient::from_schema(a_db.clone(), &client_schema, "scope")
        .await
        .unwrap();
    let b = SyncClient::from_schema(b_db, &client_schema, "scope")
        .await
        .unwrap();
    assert!(
        SyncClient::from_schema(server_db.clone(), &server_schema, "scope")
            .await
            .is_err()
    );
    assert!(
        SyncServer::from_schema(
            a_db.clone(),
            &client_schema,
            "scope",
            "principal",
            Arc::new(AllowAll)
        )
        .await
        .is_err()
    );
    assert_eq!(a.database().model_role::<Task>(), Some(ModelRole::Client));
    assert_eq!(
        server.database().model_role::<Task>(),
        Some(ModelRole::Server)
    );
    assert_eq!(a.database().model_role::<Setting>(), None);
    let transport = RestLikeTransport::new(server.for_model::<Task>().unwrap());
    let engine = a.model::<Task, _>(transport.clone()).unwrap();
    assert_eq!(
        engine.pull().await.unwrap().load_required,
        Some(LoadRequired::Initial)
    );
    assert_eq!(
        engine.flush().await.unwrap().load_required,
        Some(LoadRequired::Initial)
    );
    assert_eq!(transport.snapshots.load(Ordering::SeqCst), 0);
    assert_eq!(transport.pushes.load(Ordering::SeqCst), 0);
    engine.full_load().await.unwrap();

    let task = a
        .database()
        .create::<Task>()
        .set(Task::TITLE, "offline")
        .execute()
        .await
        .unwrap();
    assert!(!task.done);
    assert_eq!(
        a_db.query_sql("SELECT model FROM _sync_dirty", vec![])
            .await
            .unwrap()
            .len(),
        1
    );
    engine.pull().await.unwrap();
    assert_eq!(transport.pushes.load(Ordering::SeqCst), 0);
    assert!(
        server
            .database()
            .get::<Task>(task.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(engine.flush().await.unwrap().accepted_operations, 1);
    assert_eq!(transport.pulls.load(Ordering::SeqCst), 1);

    let b_engine =
        SyncEngine::for_model::<Task>(b.clone(), server.for_model::<Task>().unwrap()).unwrap();
    b_engine.full_load().await.unwrap();
    a.database()
        .update::<Task>(task.id)
        .set(Task::TITLE, "local title")
        .execute()
        .await
        .unwrap();
    ValidatedWrite::Update(
        Task::update()
            .filter(Task::ID.eq(task.id))
            .set(Task::BODY, "server body"),
    )
    .save(server.database())
    .await
    .unwrap();
    engine.pull().await.unwrap();
    let merged = a.database().get::<Task>(task.id).await.unwrap().unwrap();
    assert_eq!(merged.title, "local title");
    assert_eq!(merged.body, "server body");
    assert_eq!(transport.pushes.load(Ordering::SeqCst), 1);
    assert_eq!(transport.snapshots.load(Ordering::SeqCst), 1);

    transport.lose_reply.store(true, Ordering::SeqCst);
    assert!(engine.flush().await.is_err());
    a.database()
        .update::<Task>(task.id)
        .set(Task::DONE, true)
        .execute()
        .await
        .unwrap();
    assert_eq!(
        a_db.query_sql("SELECT operation_id FROM _sync_outbox", vec![])
            .await
            .unwrap()
            .len(),
        1
    );
    let pushes = transport.pushes.load(Ordering::SeqCst);
    engine.pull().await.unwrap();
    assert_eq!(transport.pushes.load(Ordering::SeqCst), pushes);
    assert_eq!(
        a_db.query_sql("SELECT operation_id FROM _sync_outbox", vec![])
            .await
            .unwrap()
            .len(),
        1
    );
    engine.flush().await.unwrap();
    while engine.flush().await.unwrap().has_more {}
    let received = server
        .database()
        .get::<Task>(task.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received.title, "local title");
    assert!(received.done);
    while b_engine.pull().await.unwrap().has_more {}
    assert!(
        b.database()
            .get::<Task>(task.id)
            .await
            .unwrap()
            .unwrap()
            .done
    );

    // No new journal entries or dirty rows survive a transaction rollback.
    let count = server_db
        .query_sql("SELECT sequence FROM _sync_changes", vec![])
        .await
        .unwrap()
        .len();
    let rollback = server
        .database()
        .transaction(|tx| {
            Box::pin(async move {
                tx.create::<Task>()
                    .set(Task::TITLE, "rolled back")
                    .execute()
                    .await?;
                Err::<(), _>(OrmError::Configuration("rollback".into()))
            })
        })
        .await;
    assert!(rollback.is_err());
    assert_eq!(
        server_db
            .query_sql("SELECT sequence FROM _sync_changes", vec![])
            .await
            .unwrap()
            .len(),
        count
    );
    let dirty = a_db
        .query_sql("SELECT entity_id FROM _sync_dirty", vec![])
        .await
        .unwrap()
        .len();
    assert!(
        a.database()
            .transaction(|tx| Box::pin(async move {
                tx.create::<Task>()
                    .set(Task::TITLE, "rolled back")
                    .execute()
                    .await?;
                Err::<(), _>(OrmError::Configuration("rollback".into()))
            }))
            .await
            .is_err()
    );
    assert_eq!(
        a_db.query_sql("SELECT entity_id FROM _sync_dirty", vec![])
            .await
            .unwrap()
            .len(),
        dirty
    );

    // Ordinary local tables do not enter synchronization.
    a.database()
        .create::<Setting>()
        .set(Setting::VALUE, "local only")
        .execute()
        .await
        .unwrap();
    assert_eq!(
        a_db.query_sql("SELECT entity_id FROM _sync_dirty", vec![])
            .await
            .unwrap()
            .len(),
        dirty
    );

    // Both roles capture cascades, and ORM reads/counts hide server tombstones.
    let child = server
        .database()
        .create::<Child>()
        .set(Child::TASK_ID, task.id)
        .set(Child::LABEL, "child")
        .execute()
        .await
        .unwrap();
    let child_engine = a
        .model::<Child, _>(server.for_model::<Child>().unwrap())
        .unwrap();
    child_engine.full_load().await.unwrap();
    a.database().delete::<Task>(task.id).await.unwrap();
    assert!(a.database().get::<Child>(child.id).await.unwrap().is_none());
    assert!(server.database().delete::<Task>(task.id).await.unwrap());
    assert!(
        server
            .database()
            .get::<Child>(child.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(server.database().count::<Task>().await.unwrap(), 0);
    assert_eq!(server.database().count::<Child>().await.unwrap(), 0);

    // Pruning the journal requires an explicit full load, never an implicit snapshot.
    let later = server
        .database()
        .create::<Task>()
        .set(Task::TITLE, "later")
        .execute()
        .await
        .unwrap();
    server_db
        .execute_sql("DELETE FROM _sync_changes", vec![])
        .await
        .unwrap();
    assert_eq!(
        engine.pull().await.unwrap().load_required,
        Some(LoadRequired::Reset)
    );
    assert_eq!(transport.snapshots.load(Ordering::SeqCst), 1);
    engine.full_load().await.unwrap();
    assert!(a.database().get::<Task>(later.id).await.unwrap().is_some());
    assert_eq!(transport.snapshots.load(Ordering::SeqCst), 2);

    for (admin, name) in [server_cleanup, a_cleanup, b_cleanup].into_iter().flatten() {
        admin
            .execute_sql(format!("DROP SCHEMA \"{name}\" CASCADE"), vec![])
            .await
            .unwrap();
    }
}
