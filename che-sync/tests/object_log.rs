use che_orm::{
    Database, Migration, MigrationGraph, MigrationId, MigrationOperation, Model, OrmError,
    ProjectState, SchemaSet, Transaction, TransactionFuture, Uuid, ValidatedWrite,
};
use che_sync::{
    AllowAll, ModelLogClient, ModelLogTransport, Registry, SchemaRole, SyncServer, TransportFuture,
    model_log::*, object_log::*,
};
use std::sync::Arc;
#[derive(Debug, Model)]
#[orm(table = "object_messages", sync, sync_name = "messages.message")]
struct Message {
    #[orm(primary_key)]
    id: Uuid,
    title: String,
}
#[derive(Debug, Model)]
#[orm(table = "object_test_grants", unique("principal", "object_id"))]
struct Grant {
    #[orm(primary_key)]
    id: i64,
    principal: String,
    object_id: Uuid,
    readable: bool,
    changeable: bool,
}
fn schema(role: SchemaRole) -> SchemaSet {
    match role {
        SchemaRole::Client => SchemaSet::new().client_model::<Message>(),
        SchemaRole::Server => SchemaSet::new().server_model::<Message>().model::<Grant>(),
    }
}
fn registry(role: SchemaRole) -> Registry {
    Registry::from_schema(&schema(role)).unwrap()
}
async fn migrate(db: &Database, role: SchemaRole) {
    let state = ProjectState::from_app_schemas([("messages", &schema(role))]).unwrap();
    let first = MigrationId {
        app: "messages".into(),
        name: "0001".into(),
    };
    let history = vec![
        Migration::new(
            first.clone(),
            vec![],
            state
                .models()
                .iter()
                .cloned()
                .map(|model| MigrationOperation::CreateModel { model })
                .collect(),
        ),
        Migration::new(
            MigrationId {
                app: "messages".into(),
                name: "0002_objects".into(),
            },
            vec![first],
            migration_operations(role),
        ),
    ];
    db.migrate(MigrationGraph::new(history.clone()).unwrap())
        .await
        .unwrap();
    db.migrate(MigrationGraph::new(history).unwrap())
        .await
        .unwrap();
}
struct Fixture {
    server: Database,
    client: Database,
    cleanup: Option<(Database, [String; 2])>,
}
impl Fixture {
    async fn new() -> Self {
        #[cfg(feature = "sqlite")]
        let (server, client, cleanup) = (
            Database::connect_in_memory().await.unwrap(),
            Database::connect_in_memory().await.unwrap(),
            None,
        );
        #[cfg(feature = "postgres")]
        let (server, client, cleanup) = {
            let url = std::env::var("TEST_POSTGRES_URL").expect("set TEST_POSTGRES_URL");
            let admin = Database::connect(url.clone()).await.unwrap();
            let names = [
                format!("object_server_{}", Uuid::now_v7().simple()),
                format!("object_client_{}", Uuid::now_v7().simple()),
            ];
            let mut databases = Vec::new();
            for name in &names {
                admin
                    .execute_sql(format!("CREATE SCHEMA \"{name}\""), vec![])
                    .await
                    .unwrap();
                let sep = if url.contains('?') { '&' } else { '?' };
                databases.push(
                    Database::connect(format!("{url}{sep}options=-csearch_path%3D{name}"))
                        .await
                        .unwrap(),
                );
            }
            (
                databases.remove(0),
                databases.remove(0),
                Some((admin, names)),
            )
        };
        migrate(&server, SchemaRole::Server).await;
        migrate(&client, SchemaRole::Client).await;
        Self {
            server,
            client,
            cleanup,
        }
    }
    async fn cleanup(self) {
        if let Some((admin, names)) = self.cleanup {
            for name in names {
                admin
                    .execute_sql(format!("DROP SCHEMA \"{name}\" CASCADE"), vec![])
                    .await
                    .unwrap();
            }
        }
    }
}
struct Policy(&'static str);
impl ObjectAccessPolicy for Policy {
    fn rights<'a>(
        &'a self,
        tx: &'a Transaction,
        _: &'a str,
        id: Uuid,
    ) -> TransactionFuture<'a, ObjectRights> {
        Box::pin(async move {
            let grant = tx
                .fetch_one(
                    Grant::query().filter(Grant::PRINCIPAL.eq(self.0).and(Grant::OBJECT_ID.eq(id))),
                )
                .await?;
            Ok(grant.map_or(ObjectRights::default(), |g| ObjectRights {
                read: g.readable,
                change: g.changeable,
                delete: false,
            }))
        })
    }
    fn can_create<'a>(&'a self, _: &'a Transaction, _: &'a str) -> TransactionFuture<'a, bool> {
        Box::pin(async { Ok(false) })
    }
}
impl che_sync::AccessPolicy for Policy {
    fn authorize_scope(&self, _: &str, _: &str) -> Result<(), OrmError> {
        Ok(())
    }
    fn validate(
        &self,
        _: &str,
        _: &str,
        _: &che_sync::protocol::Entity,
        _: &che_sync::protocol::Entity,
    ) -> Result<(), OrmError> {
        Ok(())
    }
    fn authorize_write<'a>(
        &'a self,
        tx: &'a Transaction,
        current: &'a che_sync::protocol::Entity,
        mutation: &'a che_sync::protocol::Mutation,
    ) -> TransactionFuture<'a, ()> {
        Box::pin(async move {
            if !self.rights(tx, &mutation.model, current.id).await?.change {
                return Err(OrmError::Configuration(
                    "change denied in committing transaction".into(),
                ));
            }
            Ok(())
        })
    }
}
struct ProjectionPolicy;
impl ObjectAccessPolicy for ProjectionPolicy {
    fn feed_namespace(&self) -> &str {
        "another-projection"
    }
    fn rights<'a>(
        &'a self,
        tx: &'a Transaction,
        model: &'a str,
        id: Uuid,
    ) -> TransactionFuture<'a, ObjectRights> {
        Box::pin(async move { Policy("bob").rights(tx, model, id).await })
    }
    fn can_create<'a>(&'a self, _: &'a Transaction, _: &'a str) -> TransactionFuture<'a, bool> {
        Box::pin(async { Ok(false) })
    }
}
struct Transport {
    server: SyncServer,
    policy: Arc<dyn ObjectAccessPolicy>,
}
impl ModelLogTransport for Transport {
    fn id(&self) -> &'static str {
        "test.objects"
    }
    fn write<'a>(
        &'a self,
        _: &'a ModelWrite,
        _: &'a serde_json::Value,
    ) -> TransportFuture<'a, ModelWriteResult> {
        Box::pin(async { Err(OrmError::Configuration("read only".into())) })
    }
    fn changes<'a>(&'a self, _: &'a str, _: i64, _: u32) -> TransportFuture<'a, ModelChanges> {
        Box::pin(async { Err(OrmError::Configuration("read only".into())) })
    }
    fn object_changes<'a>(
        &'a self,
        _: &'a str,
        cursor: Option<ObjectCursor>,
        limit: u32,
    ) -> TransportFuture<'a, ObjectChanges> {
        Box::pin(async move {
            self.server
                .object_changes::<Message>(cursor, limit, self.policy.clone())
                .await
        })
    }
}
#[tokio::test]
#[cfg_attr(
    feature = "postgres",
    ignore = "requires TEST_POSTGRES_URL and live PostgreSQL"
)]
async fn current_state_delivery_revoke_regrant_and_retained_client_drafts() {
    let f = Fixture::new().await;
    let server = SyncServer::open_model_logs(
        f.server.clone(),
        registry(SchemaRole::Server),
        "bob",
        Arc::new(AllowAll),
    )
    .await
    .unwrap();
    let id = Uuid::now_v7();
    server
        .write(ValidatedWrite::Create(
            Message::insert()
                .set(Message::ID, id)
                .set(Message::TITLE, "private old value"),
        ))
        .await
        .unwrap();
    server
        .write(ValidatedWrite::Update(
            Message::update()
                .set(Message::TITLE, "current state")
                .filter(Message::ID.eq(id)),
        ))
        .await
        .unwrap();
    let policy = Arc::new(Policy("bob"));
    let none = server
        .object_changes::<Message>(None, 1, policy.clone())
        .await
        .unwrap();
    assert!(none.events.is_empty());
    f.server
        .create::<Grant>()
        .set(Grant::PRINCIPAL, "bob")
        .set(Grant::OBJECT_ID, id)
        .set(Grant::READABLE, true)
        .set(Grant::CHANGEABLE, true)
        .execute()
        .await
        .unwrap();
    let page = server
        .object_changes::<Message>(Some(none.cursor), 1, policy.clone())
        .await
        .unwrap();
    assert_eq!(page.events.len(), 1);
    assert!(matches!(&page.events[0].change,ObjectChange::State{state,..} if state.revision==2));
    assert!(
        !serde_json::to_string(&page)
            .unwrap()
            .contains("private old value")
    );
    let client = ModelLogClient::open_objects(
        f.client.clone(),
        registry(SchemaRole::Client),
        "server",
        "bob",
    )
    .await
    .unwrap();
    let engine = client.engine(Transport {
        server: server.clone(),
        policy: policy.clone(),
    });
    assert!(!client.has_initial_pull("messages.message").await.unwrap());
    engine.pull().await.unwrap();
    assert!(client.has_initial_pull("messages.message").await.unwrap());
    let mut draft = client.database().get::<Message>(id).await.unwrap().unwrap();
    draft.title = "local draft".into();
    client.save(&draft).await.unwrap();
    assert_eq!(
        client.pending_ids("messages.message").await.unwrap(),
        vec![id]
    );
    f.server
        .execute(
            Grant::update()
                .set(Grant::READABLE, false)
                .filter(Grant::OBJECT_ID.eq(id))
                .into_ast()
                .unwrap(),
        )
        .await
        .unwrap();
    engine.pull().await.unwrap();
    assert!(
        client
            .database()
            .get::<Message>(id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(client.save(&draft).await.is_err());
    assert!(
        client
            .retained_draft("messages.message", id)
            .await
            .unwrap()
            .is_some()
    );
    let replay = server
        .object_changes::<Message>(None, 1, policy.clone())
        .await
        .unwrap();
    assert!(matches!(
        replay.events[0].change,
        ObjectChange::Revoked { .. }
    ));
    f.server
        .execute(
            Grant::update()
                .set(Grant::READABLE, true)
                .filter(Grant::OBJECT_ID.eq(id))
                .into_ast()
                .unwrap(),
        )
        .await
        .unwrap();
    engine.pull().await.unwrap();
    assert_eq!(client.object_status().await.unwrap()[0].status, "review");
    client.resume_object("messages.message", id).await.unwrap();
    assert_eq!(client.object_status().await.unwrap()[0].status, "active");
    assert!(
        client
            .apply(server.changes::<Message>(0, 100).await.unwrap())
            .await
            .is_err()
    );
    let other = SyncServer::open_model_logs(
        f.server.clone(),
        registry(SchemaRole::Server),
        "carol",
        Arc::new(AllowAll),
    )
    .await
    .unwrap();
    assert!(
        other
            .object_changes::<Message>(Some(page.cursor.clone()), 1, Arc::new(Policy("carol")))
            .await
            .is_err()
    );
    let separate = server
        .object_changes::<Message>(None, 100, Arc::new(ProjectionPolicy))
        .await
        .unwrap();
    assert_ne!(separate.cursor.feed, page.cursor.feed);
    assert!(
        server
            .object_changes::<Message>(Some(page.cursor.clone()), 100, Arc::new(ProjectionPolicy))
            .await
            .is_err()
    );
    let guarded = SyncServer::open_model_logs(
        f.server.clone(),
        registry(SchemaRole::Server),
        "bob",
        Arc::new(Policy("bob")),
    )
    .await
    .unwrap();
    let request = guarded
        .prepare_conditional_write(
            ValidatedWrite::Update(
                Message::update()
                    .set(Message::TITLE, "must not commit")
                    .filter(Message::ID.eq(id)),
            ),
            2,
        )
        .await
        .unwrap();
    let before = guarded.prepare_push(&request).await.unwrap();
    f.server
        .execute(
            Grant::update()
                .set(Grant::CHANGEABLE, false)
                .filter(Grant::OBJECT_ID.eq(id))
                .into_ast()
                .unwrap(),
        )
        .await
        .unwrap();
    let rejected = guarded
        .write_prepared_request(
            request.clone(),
            before.clone(),
            vec![],
            "stable-request".into(),
        )
        .await
        .unwrap();
    assert!(matches!(
        &rejected,
        che_sync::protocol::PushResult::Rejected { .. }
    ));
    assert_eq!(
        f.server.get::<Message>(id).await.unwrap().unwrap().title,
        "current state"
    );
    f.server
        .execute(
            Grant::update()
                .set(Grant::CHANGEABLE, true)
                .filter(Grant::OBJECT_ID.eq(id))
                .into_ast()
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        guarded
            .write_prepared_request(request, before, vec![], "stable-request".into())
            .await
            .unwrap(),
        rejected
    );
    f.cleanup().await;
}
