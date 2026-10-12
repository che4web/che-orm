#![cfg(feature = "sqlite")]

use che_orm::{
    Database, Migration, MigrationGraph, MigrationId, MigrationOperation, Model, OrmError,
    TableState, Uuid,
};
use che_sync::{
    AllowAll, Field, Registry, Resolution, SchemaRole, SyncClient, SyncEngine, SyncModel,
    SyncServer, SyncTransport, TransportFuture, protocol::*,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Clone, Model)]
#[orm(table = "tasks")]
struct Task {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(db_column = "task_title", unique)]
    title: String,
    description: String,
    #[orm(auto_now_add)]
    created: che_orm::time::OffsetDateTime,
    #[orm(auto_now)]
    updated: che_orm::time::OffsetDateTime,
}

impl SyncModel for Task {
    const SYNC_NAME: &'static str = "tasks.task";
    fn sync_fields() -> Vec<Field> {
        vec![
            Field::server_owned("id", "id"),
            Field::writable("title", "task_title"),
            Field::writable("description", "description"),
            Field::server_owned("created", "created"),
            Field::server_owned("updated", "updated"),
        ]
    }
}

#[derive(Debug, Clone, Model)]
#[orm(table = "comments")]
struct Comment {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(foreign_key = Task, on_delete = "cascade")]
    task_id: Uuid,
    body: String,
}

impl SyncModel for Comment {
    const SYNC_NAME: &'static str = "tasks.comment";
    fn sync_fields() -> Vec<Field> {
        vec![
            Field::server_owned("id", "id"),
            Field::writable("task", "task_id"),
            Field::writable("body", "body"),
        ]
    }
}

#[derive(Debug, Clone, Model)]
#[orm(table = "optional_comments")]
struct OptionalComment {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(foreign_key = Task, on_delete = "set null")]
    task_id: Option<Uuid>,
    body: String,
}

impl SyncModel for OptionalComment {
    const SYNC_NAME: &'static str = "tasks.optional_comment";
    fn sync_fields() -> Vec<Field> {
        vec![
            Field::server_owned("id", "id"),
            Field::writable("task", "task_id"),
            Field::writable("body", "body"),
        ]
    }
}

#[derive(Debug, Clone, Model)]
#[orm(table = "assets")]
struct Asset {
    #[orm(primary_key)]
    id: Uuid,
    bytes: Vec<u8>,
    enabled: bool,
    score: f64,
    count: Option<i64>,
    scheduled: Option<che_orm::time::OffsetDateTime>,
    image: Option<che_orm::ImageField>,
}

impl SyncModel for Asset {
    const SYNC_NAME: &'static str = "assets.asset";
    fn sync_fields() -> Vec<Field> {
        vec![
            Field::server_owned("id", "id"),
            Field::writable("bytes", "bytes"),
            Field::writable("enabled", "enabled"),
            Field::writable("score", "score"),
            Field::writable("count", "count"),
            Field::writable("scheduled", "scheduled"),
            Field::server_owned("image", "image"),
        ]
    }
}

#[derive(Debug, Clone, Model)]
#[orm(table = "owned_slots")]
struct OwnedSlot {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(unique, default = "1")]
    slot: i64,
    title: String,
    scheduled: Option<che_orm::time::OffsetDateTime>,
}

impl SyncModel for OwnedSlot {
    const SYNC_NAME: &'static str = "owned.slot";
    fn sync_fields() -> Vec<Field> {
        vec![
            Field::server_owned("id", "id"),
            Field::server_owned("slot", "slot"),
            Field::writable("title", "title"),
            Field::writable("scheduled", "scheduled"),
        ]
    }
}

fn registry() -> Registry {
    Registry::new()
        .register::<Task>()
        .unwrap()
        .register::<Comment>()
        .unwrap()
        .register::<OptionalComment>()
        .unwrap()
        .register::<Asset>()
        .unwrap()
        .register::<OwnedSlot>()
        .unwrap()
}
fn task(title: &str) -> Task {
    let time = che_orm::time::OffsetDateTime::UNIX_EPOCH;
    Task {
        id: Uuid::now_v7(),
        title: title.into(),
        description: "original".into(),
        created: time,
        updated: time,
    }
}

async fn database(role: SchemaRole) -> Database {
    let db = Database::connect_in_memory().await.unwrap();
    migrate(&db, role).await;
    db
}

async fn migrate(db: &Database, role: SchemaRole) {
    let mut operations = vec![
        MigrationOperation::CreateTable {
            table: TableState::from(&Task::schema()),
        },
        MigrationOperation::CreateTable {
            table: TableState::from(&Comment::schema()),
        },
    ];
    operations.extend([
        MigrationOperation::CreateTable {
            table: TableState::from(&OptionalComment::schema()),
        },
        MigrationOperation::CreateTable {
            table: TableState::from(&Asset::schema()),
        },
        MigrationOperation::CreateTable {
            table: TableState::from(&OwnedSlot::schema()),
        },
    ]);
    operations.extend(registry().migration_operations(role));
    db.migrate(
        MigrationGraph::new(vec![Migration::new(
            MigrationId {
                app: "sync_test".into(),
                name: "0001".into(),
            },
            vec![],
            operations,
        )])
        .unwrap(),
    )
    .await
    .unwrap();
}

async fn server() -> SyncServer {
    SyncServer::open(
        database(SchemaRole::Server).await,
        registry(),
        "workspace",
        "user",
        Arc::new(AllowAll),
    )
    .await
    .unwrap()
}
async fn client() -> SyncClient {
    SyncClient::open(database(SchemaRole::Client).await, registry(), "workspace")
        .await
        .unwrap()
}

async fn sync(client: &SyncClient, server: &SyncServer) {
    let engine = SyncEngine::new(client.clone(), server.clone());
    for _ in 0..10 {
        if !engine.sync_once().await.unwrap().has_more {
            return;
        }
    }
    panic!("sync did not converge");
}

#[tokio::test]
async fn offline_create_update_delete_and_managed_values() {
    let server = server().await;
    let a = client().await;
    let b = client().await;
    let mut t = task("first");
    a.save(&t).await.unwrap();
    assert_eq!(
        a.database().get::<Task>(t.id).await.unwrap().unwrap().title,
        "first"
    );
    sync(&a, &server).await;
    sync(&b, &server).await;
    let confirmed = a.database().get::<Task>(t.id).await.unwrap().unwrap();
    assert_ne!(confirmed.created, t.created);
    assert_eq!(
        confirmed.created,
        b.database()
            .get::<Task>(t.id)
            .await
            .unwrap()
            .unwrap()
            .created
    );
    t = confirmed;
    t.title = "edited".into();
    a.save(&t).await.unwrap();
    sync(&a, &server).await;
    sync(&b, &server).await;
    assert_eq!(
        b.database().get::<Task>(t.id).await.unwrap().unwrap().title,
        "edited"
    );
    a.delete::<Task>(t.id).await.unwrap();
    sync(&a, &server).await;
    sync(&b, &server).await;
    assert!(b.database().get::<Task>(t.id).await.unwrap().is_none());
    assert!(
        server.snapshot().await.unwrap().entities[0]
            .record
            .is_none()
    );
}

#[tokio::test]
async fn disjoint_fields_merge_and_same_field_conflicts_are_resolved() {
    let server = server().await;
    let a = client().await;
    let b = client().await;
    let t = task("base");
    a.save(&t).await.unwrap();
    sync(&a, &server).await;
    sync(&b, &server).await;
    let mut at = a.database().get::<Task>(t.id).await.unwrap().unwrap();
    let mut bt = b.database().get::<Task>(t.id).await.unwrap().unwrap();
    at.title = "A".into();
    bt.description = "B".into();
    a.save(&at).await.unwrap();
    b.save(&bt).await.unwrap();
    sync(&a, &server).await;
    sync(&b, &server).await;
    sync(&a, &server).await;
    let merged = a.database().get::<Task>(t.id).await.unwrap().unwrap();
    assert_eq!((&*merged.title, &*merged.description), ("A", "B"));
    at = merged;
    bt = b.database().get::<Task>(t.id).await.unwrap().unwrap();
    at.title = "A2".into();
    bt.title = "B2".into();
    a.save(&at).await.unwrap();
    b.save(&bt).await.unwrap();
    sync(&a, &server).await;
    sync(&b, &server).await;
    assert_eq!(b.conflicts().await.unwrap()[0].fields, vec!["title"]);
    assert_eq!(
        b.database().get::<Task>(t.id).await.unwrap().unwrap().title,
        "B2"
    );
    b.resolve(Task::SYNC_NAME, t.id, Resolution::Local)
        .await
        .unwrap();
    sync(&b, &server).await;
    sync(&a, &server).await;
    assert_eq!(
        a.database().get::<Task>(t.id).await.unwrap().unwrap().title,
        "B2"
    );
}

struct LostReply {
    server: SyncServer,
    lose: AtomicBool,
}
impl SyncTransport for LostReply {
    fn push(&self, request: PushRequest) -> TransportFuture<'_, PushResult> {
        Box::pin(async move {
            let result = self.server.push(request).await?;
            if self.lose.swap(false, Ordering::SeqCst) {
                return Err(OrmError::Interaction("lost response".into()));
            }
            Ok(result)
        })
    }
    fn pull(&self, request: PullRequest) -> TransportFuture<'_, PullResult> {
        Box::pin(self.server.pull(request))
    }
    fn snapshot(&self) -> TransportFuture<'_, Snapshot> {
        Box::pin(self.server.snapshot())
    }
}

#[tokio::test]
async fn lost_reply_reopen_retry_does_not_duplicate_or_overwrite_new_edits() {
    let server = server().await;
    let a = client().await;
    let mut t = task("sent");
    a.save(&t).await.unwrap();
    let engine = SyncEngine::new(
        a.clone(),
        LostReply {
            server: server.clone(),
            lose: AtomicBool::new(true),
        },
    );
    assert!(engine.sync_once().await.is_err());
    assert_eq!(server.snapshot().await.unwrap().entities[0].revision, 1);
    t.title = "new edit".into();
    a.save(&t).await.unwrap();
    let reopened = SyncClient::open(a.database().clone(), registry(), "workspace")
        .await
        .unwrap();
    assert_eq!(reopened.replica_id(), a.replica_id());
    sync(&reopened, &server).await;
    let snapshot = server.snapshot().await.unwrap();
    assert_eq!(snapshot.entities[0].revision, 2);
    assert_eq!(
        snapshot.entities[0].record.as_ref().unwrap()["title"],
        Value::Text("new edit".into())
    );
    assert!(reopened.conflicts().await.unwrap().is_empty());
}

struct EditDuringPush {
    server: SyncServer,
    client: SyncClient,
    edit: AtomicBool,
}
impl SyncTransport for EditDuringPush {
    fn push(&self, request: PushRequest) -> TransportFuture<'_, PushResult> {
        Box::pin(async move {
            let result = self.server.push(request.clone()).await?;
            if self.edit.swap(false, Ordering::SeqCst) {
                let mut task = self
                    .client
                    .database()
                    .get::<Task>(request.operation.mutations[0].id)
                    .await?
                    .unwrap();
                task.title = "edited in flight".into();
                self.client.save(&task).await?;
            }
            Ok(result)
        })
    }
    fn pull(&self, request: PullRequest) -> TransportFuture<'_, PullResult> {
        Box::pin(self.server.pull(request))
    }
    fn snapshot(&self) -> TransportFuture<'_, Snapshot> {
        Box::pin(self.server.snapshot())
    }
}

#[tokio::test]
async fn in_flight_edits_survive_acknowledgement() {
    let server = server().await;
    let a = client().await;
    let t = task("sent");
    a.save(&t).await.unwrap();
    let engine = SyncEngine::new(
        a.clone(),
        EditDuringPush {
            server: server.clone(),
            client: a.clone(),
            edit: AtomicBool::new(true),
        },
    );
    engine.sync_once().await.unwrap();
    assert_eq!(
        a.database().get::<Task>(t.id).await.unwrap().unwrap().title,
        "edited in flight"
    );
    assert!(a.conflicts().await.unwrap().is_empty());
    sync(&a, &server).await;
    assert_eq!(
        server.snapshot().await.unwrap().entities[0]
            .record
            .as_ref()
            .unwrap()["title"],
        Value::Text("edited in flight".into())
    );
}

#[tokio::test]
async fn related_creates_and_cascade_deletes_are_atomic() {
    let server = server().await;
    let a = client().await;
    let b = client().await;
    let t = task("parent");
    let comment = Comment {
        id: Uuid::now_v7(),
        task_id: t.id,
        body: "child".into(),
    };
    let task_id = t.id;
    let comment_id = comment.id;
    a.transaction(move |tx| {
        Box::pin(async move {
            tx.save(&t).await?;
            tx.save(&comment).await
        })
    })
    .await
    .unwrap();
    sync(&a, &server).await;
    sync(&b, &server).await;
    assert!(
        b.database()
            .get::<Comment>(comment_id)
            .await
            .unwrap()
            .is_some()
    );
    a.delete::<Task>(task_id).await.unwrap();
    sync(&a, &server).await;
    sync(&b, &server).await;
    assert!(
        b.database()
            .get::<Comment>(comment_id)
            .await
            .unwrap()
            .is_none()
    );
    let snapshot = server.snapshot().await.unwrap();
    assert_eq!(snapshot.cursor.sequence, 2);
    assert!(snapshot.entities.iter().all(|e| e.record.is_none()));
}

#[tokio::test]
async fn transaction_rollback_leaves_no_visible_or_dirty_writes() {
    let a = client().await;
    let t = task("rollback");
    let id = t.id;
    let result: Result<(), OrmError> = a
        .transaction(move |tx| {
            Box::pin(async move {
                tx.save(&t).await?;
                Err(OrmError::Configuration("abort".into()))
            })
        })
        .await;
    assert!(result.is_err());
    assert!(a.database().get::<Task>(id).await.unwrap().is_none());
    assert!(
        a.database()
            .query_sql("SELECT model FROM _sync_dirty", vec![])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn delete_update_conflict_cannot_resurrect_deleted_uuid() {
    let server = server().await;
    let a = client().await;
    let b = client().await;
    let t = task("delete");
    a.save(&t).await.unwrap();
    sync(&a, &server).await;
    sync(&b, &server).await;
    let mut edited = b.database().get::<Task>(t.id).await.unwrap().unwrap();
    edited.title = "offline edit".into();
    b.save(&edited).await.unwrap();
    a.delete::<Task>(t.id).await.unwrap();
    sync(&a, &server).await;
    sync(&b, &server).await;
    assert_eq!(b.conflicts().await.unwrap()[0].fields, vec!["$entity"]);
    assert!(
        b.resolve(Task::SYNC_NAME, t.id, Resolution::Local)
            .await
            .is_err()
    );
    b.resolve(Task::SYNC_NAME, t.id, Resolution::Remote)
        .await
        .unwrap();
    sync(&b, &server).await;
    assert!(b.database().get::<Task>(t.id).await.unwrap().is_none());
}

#[tokio::test]
async fn server_writes_and_small_pull_pages_converge() {
    let server = server().await;
    let a = client().await;
    SyncEngine::new(a.clone(), server.clone())
        .sync_once()
        .await
        .unwrap();
    for name in ["one", "two", "three"] {
        assert!(matches!(
            server.save(&task(name)).await.unwrap(),
            PushResult::Accepted(_)
        ));
    }
    let engine = SyncEngine::new(a.clone(), server)
        .with_page_size(1)
        .unwrap();
    assert!(engine.sync_once().await.unwrap().has_more);
    for _ in 0..5 {
        if !engine.sync_once().await.unwrap().has_more {
            break;
        }
    }
    assert_eq!(a.database().all::<Task>().await.unwrap().len(), 3);
}

#[tokio::test]
async fn rejected_group_rolls_back_all_rows_and_journal() {
    let server = server().await;
    let id1 = Uuid::now_v7();
    let id2 = Uuid::now_v7();
    let operation = Operation {
        id: Uuid::now_v7(),
        mutations: [id1, id2]
            .iter()
            .map(|id| Mutation {
                model: Task::SYNC_NAME.into(),
                id: *id,
                schema_version: 1,
                base_revision: 0,
                changes: Some(Record::from([
                    ("title".into(), Value::Text("duplicate".into())),
                    ("description".into(), Value::Text("x".into())),
                ])),
            })
            .collect(),
    };
    let request = PushRequest {
        protocol_version: 1,
        scope: "workspace".into(),
        replica_id: Uuid::now_v7(),
        operation,
    };
    let result = server.push(request.clone()).await.unwrap();
    assert!(matches!(result, PushResult::Rejected { .. }));
    assert_eq!(result, server.push(request).await.unwrap());
    let snapshot = server.snapshot().await.unwrap();
    assert!(snapshot.entities.is_empty());
    assert_eq!(snapshot.cursor.sequence, 0);
}

#[tokio::test]
async fn changed_operation_identity_is_rejected_and_scope_is_bound() {
    let server = server().await;
    let request = PushRequest {
        protocol_version: 1,
        scope: "workspace".into(),
        replica_id: Uuid::now_v7(),
        operation: Operation {
            id: Uuid::now_v7(),
            mutations: vec![Mutation {
                model: Task::SYNC_NAME.into(),
                id: Uuid::now_v7(),
                schema_version: 1,
                base_revision: 0,
                changes: Some(Record::from([
                    ("title".into(), Value::Text("identity".into())),
                    ("description".into(), Value::Text("x".into())),
                ])),
            }],
        },
    };
    assert!(matches!(
        server.push(request.clone()).await.unwrap(),
        PushResult::Accepted(_)
    ));
    let mut changed = request.clone();
    changed.operation.mutations[0]
        .changes
        .as_mut()
        .unwrap()
        .insert("title".into(), Value::Text("changed".into()));
    assert!(server.push(changed).await.is_err());
    let mut wrong_scope = request;
    wrong_scope.scope = "other".into();
    assert!(server.push(wrong_scope).await.is_err());
}

#[tokio::test]
async fn constraint_conflict_retains_local_data_and_remote_shadow() {
    let server = server().await;
    let a = client().await;
    let b = client().await;
    let remote = task("collision");
    let mut local = task("collision");
    a.save(&remote).await.unwrap();
    sync(&a, &server).await;
    b.save(&local).await.unwrap();
    sync(&b, &server).await;
    let conflicts = b.conflicts().await.unwrap();
    // Push-first exposes the rejected local create as well as the blocked
    // incoming row. Only the latter can be retried without explicit resolution.
    assert_eq!(conflicts.len(), 2);
    assert!(
        conflicts
            .iter()
            .any(|c| c.id == remote.id && c.retryable && c.reason.is_some())
    );
    assert!(
        conflicts
            .iter()
            .any(|c| c.id == local.id && !c.retryable && c.reason.is_some())
    );
    assert_eq!(
        b.database()
            .get::<Task>(local.id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "collision"
    );
    assert!(b.database().get::<Task>(remote.id).await.unwrap().is_none());
    local.title = "no collision".into();
    b.save(&local).await.unwrap();
    b.resolve(Task::SYNC_NAME, local.id, Resolution::Local)
        .await
        .unwrap();
    // The unchanged remote row retries automatically after its UNIQUE
    // dependency is released; it is not a competing user edit.
    sync(&b, &server).await;
    assert_eq!(b.database().all::<Task>().await.unwrap().len(), 2);
    assert!(b.conflicts().await.unwrap().is_empty());
}

#[tokio::test]
async fn remote_cascade_does_not_silently_destroy_a_local_child_edit() {
    let server = server().await;
    let a = client().await;
    let b = client().await;
    let t = task("parent conflict");
    let id = t.id;
    let comment = Comment {
        id: Uuid::now_v7(),
        task_id: id,
        body: "original".into(),
    };
    let child_id = comment.id;
    a.transaction(move |tx| {
        Box::pin(async move {
            tx.save(&t).await?;
            tx.save(&comment).await
        })
    })
    .await
    .unwrap();
    sync(&a, &server).await;
    sync(&b, &server).await;
    let mut edited = b
        .database()
        .get::<Comment>(child_id)
        .await
        .unwrap()
        .unwrap();
    edited.body = "offline child".into();
    b.save(&edited).await.unwrap();
    assert!(matches!(
        server.delete::<Task>(id).await.unwrap(),
        PushResult::Accepted(_)
    ));
    sync(&b, &server).await;
    assert_eq!(
        b.database()
            .get::<Comment>(child_id)
            .await
            .unwrap()
            .unwrap()
            .body,
        "offline child"
    );
    assert!(b.database().get::<Task>(id).await.unwrap().is_some());
    let conflicts = b.conflicts().await.unwrap();
    assert_eq!(conflicts.len(), 2);
    let child_conflict = conflicts.iter().find(|c| c.id == child_id).unwrap();
    assert_eq!(
        child_conflict.base.as_ref().unwrap()["body"],
        Value::Text("original".into())
    );
    assert_eq!(
        child_conflict.local.as_ref().unwrap()["body"],
        Value::Text("offline child".into())
    );
    assert_eq!(child_conflict.fields, vec!["$entity"]);
    // Empty pages also retry the parent; none may erase the child's base.
    sync(&b, &server).await;
    let conflicts = b.conflicts().await.unwrap();
    assert_eq!(
        conflicts.iter().find(|c| c.id == child_id).unwrap().base,
        child_conflict.base
    );
    b.resolve(Comment::SYNC_NAME, child_id, Resolution::Remote)
        .await
        .unwrap();
    b.resolve(Task::SYNC_NAME, id, Resolution::Remote)
        .await
        .unwrap();
    sync(&b, &server).await;
    assert!(
        b.database()
            .get::<Comment>(child_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn history_gap_resets_to_snapshot_and_preserves_offline_edits() {
    let db = database(SchemaRole::Server).await;
    let server = SyncServer::open(
        db.clone(),
        registry(),
        "workspace",
        "user",
        Arc::new(AllowAll),
    )
    .await
    .unwrap();
    let a = client().await;
    let b = client().await;
    let t = task("history");
    a.save(&t).await.unwrap();
    sync(&a, &server).await;
    sync(&b, &server).await;
    let mut at = a.database().get::<Task>(t.id).await.unwrap().unwrap();
    let mut bt = b.database().get::<Task>(t.id).await.unwrap().unwrap();
    bt.description = "offline survives".into();
    b.save(&bt).await.unwrap();
    for title in ["history 2", "history 3", "remote history"] {
        at.title = title.into();
        a.save(&at).await.unwrap();
        sync(&a, &server).await;
    }
    // Leave the first event contiguous and remove an interval inside the page.
    db.execute_sql("DELETE FROM _sync_changes WHERE sequence = 3", vec![])
        .await
        .unwrap();
    sync(&b, &server).await;
    let merged = b.database().get::<Task>(t.id).await.unwrap().unwrap();
    assert_eq!(
        (&*merged.title, &*merged.description),
        ("remote history", "offline survives")
    );
    assert!(b.conflicts().await.unwrap().is_empty());
}

#[tokio::test]
async fn stale_revision_conflicts_and_server_owned_fields_are_rejected() {
    let server = server().await;
    let t = task("revision");
    assert!(matches!(
        server.save(&t).await.unwrap(),
        PushResult::Accepted(_)
    ));
    let mut request = PushRequest {
        protocol_version: 1,
        scope: "workspace".into(),
        replica_id: Uuid::now_v7(),
        operation: Operation {
            id: Uuid::now_v7(),
            mutations: vec![Mutation {
                model: Task::SYNC_NAME.into(),
                id: t.id,
                schema_version: 1,
                base_revision: 0,
                changes: Some(Record::from([(
                    "title".into(),
                    Value::Text("stale".into()),
                )])),
            }],
        },
    };
    let result = server.push(request.clone()).await.unwrap();
    assert!(matches!(result, PushResult::Conflict(ref entities) if entities[0].revision == 1));
    request.operation.id = Uuid::now_v7();
    request.operation.mutations[0].base_revision = 1;
    request.operation.mutations[0].changes = Some(Record::from([(
        "created".into(),
        Value::DateTime(t.created),
    )]));
    assert!(matches!(
        server.push(request).await.unwrap(),
        PushResult::Rejected { .. }
    ));
}

#[tokio::test]
async fn denied_policy_rolls_back_cascade_and_does_not_advance_journal() {
    struct DenyDelete;
    impl che_sync::AccessPolicy for DenyDelete {
        fn authorize_scope(&self, _: &str, _: &str) -> Result<(), OrmError> {
            Ok(())
        }
        fn validate(&self, _: &str, _: &str, _: &Entity, next: &Entity) -> Result<(), OrmError> {
            if next.model == Comment::SYNC_NAME && next.record.is_none() {
                Err(OrmError::Configuration("child deletion denied".into()))
            } else {
                Ok(())
            }
        }
    }
    let db = database(SchemaRole::Server).await;
    let server = SyncServer::open(
        db.clone(),
        registry(),
        "workspace",
        "user",
        Arc::new(AllowAll),
    )
    .await
    .unwrap();
    let a = client().await;
    let t = task("policy");
    let id = t.id;
    let comment = Comment {
        id: Uuid::now_v7(),
        task_id: id,
        body: "protected".into(),
    };
    let child_id = comment.id;
    a.transaction(move |tx| {
        Box::pin(async move {
            tx.save(&t).await?;
            tx.save(&comment).await
        })
    })
    .await
    .unwrap();
    sync(&a, &server).await;
    let restricted = SyncServer::open(
        db.clone(),
        registry(),
        "workspace",
        "user",
        Arc::new(DenyDelete),
    )
    .await
    .unwrap();
    assert!(matches!(
        restricted.delete::<Task>(id).await.unwrap(),
        PushResult::Rejected { .. }
    ));
    assert!(db.get::<Task>(id).await.unwrap().is_some());
    assert!(db.get::<Comment>(child_id).await.unwrap().is_some());
    assert_eq!(server.snapshot().await.unwrap().cursor.sequence, 1);
}

#[tokio::test]
async fn cross_scope_fk_is_rejected_without_exposing_another_scopes_data() {
    let db = database(SchemaRole::Server).await;
    let one = SyncServer::open(db.clone(), registry(), "one", "user", Arc::new(AllowAll))
        .await
        .unwrap();
    let two = SyncServer::open(db, registry(), "two", "user", Arc::new(AllowAll))
        .await
        .unwrap();
    let parent = task("private");
    one.save(&parent).await.unwrap();
    let child = Comment {
        id: Uuid::now_v7(),
        task_id: parent.id,
        body: "cross scope".into(),
    };
    assert!(matches!(
        two.save(&child).await.unwrap(),
        PushResult::Rejected { .. }
    ));
    assert!(two.snapshot().await.unwrap().entities.is_empty());
}

#[tokio::test]
async fn nullable_fk_set_null_is_included_in_server_journal() {
    let server = server().await;
    let a = client().await;
    let t = task("optional parent");
    assert!(matches!(
        server.save(&t).await.unwrap(),
        PushResult::Accepted(_)
    ));
    let child = OptionalComment {
        id: Uuid::now_v7(),
        task_id: Some(t.id),
        body: "remains".into(),
    };
    assert!(matches!(
        server.save(&child).await.unwrap(),
        PushResult::Accepted(_)
    ));
    sync(&a, &server).await;
    let result = server.delete::<Task>(t.id).await.unwrap();
    assert!(matches!(result, PushResult::Accepted(ref entities) if entities.len() == 2));
    sync(&a, &server).await;
    let remaining = a
        .database()
        .get::<OptionalComment>(child.id)
        .await
        .unwrap()
        .unwrap();
    assert!(remaining.task_id.is_none());
    assert_eq!(remaining.body, "remains");
    assert!(a.conflicts().await.unwrap().is_empty());
}

#[tokio::test]
async fn wire_roundtrip_preserves_types_and_server_owned_image_keys() {
    assert_eq!(Asset::write_protected_columns(), &["image"]);
    let server = server().await;
    let a = client().await;
    let asset = Asset {
        id: Uuid::now_v7(),
        bytes: vec![0, 255, 42],
        enabled: true,
        score: 1.25,
        count: Some(i64::MAX),
        scheduled: Some(che_orm::time::OffsetDateTime::now_utc()),
        image: Some(che_orm::ImageField::new("uploads/trusted.png")),
    };
    assert!(matches!(
        server.save(&asset).await.unwrap(),
        PushResult::Accepted(_)
    ));
    let snapshot = server.snapshot().await.unwrap();
    let json = serde_json::to_string(&snapshot).unwrap();
    assert_eq!(snapshot, serde_json::from_str::<Snapshot>(&json).unwrap());
    sync(&a, &server).await;
    let mut loaded = a.database().get::<Asset>(asset.id).await.unwrap().unwrap();
    assert_eq!(loaded.bytes, asset.bytes);
    assert_eq!(loaded.enabled, asset.enabled);
    assert_eq!(loaded.score, asset.score);
    assert_eq!(loaded.count, asset.count);
    assert_eq!(loaded.scheduled, asset.scheduled);
    assert_eq!(loaded.image, asset.image);
    loaded.image = Some(che_orm::ImageField::new("client/forged.png"));
    loaded.count = None;
    loaded.scheduled = None;
    a.save(&loaded).await.unwrap();
    sync(&a, &server).await;
    let confirmed = a.database().get::<Asset>(asset.id).await.unwrap().unwrap();
    assert_eq!(confirmed.image, asset.image);
    assert!(confirmed.count.is_none());
    assert!(confirmed.scheduled.is_none());
}

#[test]
fn image_keys_cannot_be_registered_as_writable_sync_fields() {
    #[derive(Debug, Model)]
    #[orm(table = "bad_images")]
    struct BadImage {
        #[orm(primary_key)]
        id: Uuid,
        image: Option<che_orm::ImageField>,
    }
    impl SyncModel for BadImage {
        const SYNC_NAME: &'static str = "bad.image";
        fn sync_fields() -> Vec<Field> {
            vec![
                Field::server_owned("id", "id"),
                Field::writable("image", "image"),
            ]
        }
    }
    assert!(Registry::new().register::<BadImage>().is_err());
}

#[tokio::test]
async fn generated_sync_tables_participate_in_migration_planning() {
    let registry = registry();
    let mut state = che_orm::ProjectState::default();
    registry
        .extend_migration_state(&mut state, SchemaRole::Client)
        .unwrap();
    state.validate().unwrap();
    let mut same = che_orm::ProjectState::default();
    registry
        .extend_migration_state(&mut same, SchemaRole::Client)
        .unwrap();
    assert!(state.diff(&same).unwrap().is_empty());
}

#[tokio::test]
async fn file_database_reconnect_recovers_outbox_after_lost_reply() {
    let path = std::env::temp_dir().join(format!("che-sync-{}.db", Uuid::now_v7()));
    let server = server().await;
    let db = Database::connect(path.to_string_lossy().into_owned())
        .await
        .unwrap();
    migrate(&db, SchemaRole::Client).await;
    let a = SyncClient::open(db, registry(), "workspace").await.unwrap();
    let replica = a.replica_id();
    let t = task("persisted");
    let id = t.id;
    a.save(&t).await.unwrap();
    let engine = SyncEngine::new(
        a.clone(),
        LostReply {
            server: server.clone(),
            lose: AtomicBool::new(true),
        },
    );
    assert!(engine.sync_once().await.is_err());
    // Recover an older outbox format too. The immutable request is identical;
    // redundant historical snapshots must not be required or used as the base.
    let rows = a
        .database()
        .query_sql("SELECT payload FROM _sync_outbox", vec![])
        .await
        .unwrap();
    let mut legacy: serde_json::Value =
        serde_json::from_str(&rows[0].get::<String>(0).unwrap()).unwrap();
    legacy["bases"] = serde_json::json!([Entity {
        model: Task::SYNC_NAME.into(),
        id,
        schema_version: 1,
        revision: 0,
        record: None
    }]);
    legacy["sent"] = serde_json::json!([Record::from([
        ("id".into(), Value::Uuid(id)),
        ("title".into(), Value::Text(t.title.clone())),
        ("description".into(), Value::Text(t.description.clone())),
        ("created".into(), Value::DateTime(t.created)),
        ("updated".into(), Value::DateTime(t.updated)),
    ])]);
    a.database()
        .execute_sql(
            "UPDATE _sync_outbox SET payload = $1",
            vec![che_orm::DatabaseValue::Text(
                serde_json::to_string(&legacy).unwrap(),
            )],
        )
        .await
        .unwrap();
    drop(engine);
    drop(a);
    let db = Database::connect(path.to_string_lossy().into_owned())
        .await
        .unwrap();
    let reopened = SyncClient::open(db, registry(), "workspace").await.unwrap();
    assert_eq!(reopened.replica_id(), replica);
    sync(&reopened, &server).await;
    assert_eq!(
        reopened
            .database()
            .get::<Task>(id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "persisted"
    );
    assert_eq!(server.snapshot().await.unwrap().entities[0].revision, 1);
    drop(reopened);
    std::fs::remove_file(path).unwrap();
}

struct TraceTransport {
    server: SyncServer,
    calls: Arc<std::sync::Mutex<Vec<&'static str>>>,
    lose: AtomicBool,
}

impl SyncTransport for TraceTransport {
    fn push(&self, request: PushRequest) -> TransportFuture<'_, PushResult> {
        Box::pin(async move {
            self.calls.lock().unwrap().push("push");
            let result = self.server.push(request).await?;
            if self.lose.swap(false, Ordering::SeqCst) {
                return Err(OrmError::Interaction("lost reply".into()));
            }
            Ok(result)
        })
    }
    fn pull(&self, request: PullRequest) -> TransportFuture<'_, PullResult> {
        self.calls.lock().unwrap().push("pull");
        Box::pin(self.server.pull(request))
    }
    fn snapshot(&self) -> TransportFuture<'_, Snapshot> {
        self.calls.lock().unwrap().push("snapshot");
        Box::pin(self.server.snapshot())
    }
}

#[tokio::test]
async fn push_first_freezes_one_shadow_and_outbox_has_no_server_snapshot() {
    let server = server().await;
    let a = client().await;
    let mut t = task("push first base");
    a.save(&t).await.unwrap();
    sync(&a, &server).await;
    t = a.database().get::<Task>(t.id).await.unwrap().unwrap();
    t.title = "push first sent".into();
    a.save(&t).await.unwrap();
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let engine = SyncEngine::new(
        a.clone(),
        TraceTransport {
            server: server.clone(),
            calls: calls.clone(),
            lose: AtomicBool::new(true),
        },
    );
    assert!(engine.sync_once().await.is_err());
    assert_eq!(*calls.lock().unwrap(), vec!["push"]);
    let rows = a
        .database()
        .query_sql("SELECT payload FROM _sync_outbox", vec![])
        .await
        .unwrap();
    let pending: serde_json::Value =
        serde_json::from_str(&rows[0].get::<String>(0).unwrap()).unwrap();
    assert!(pending.get("bases").is_none());
    assert!(pending.get("sent").is_none());
    assert_eq!(
        pending["request"]["operation"]["mutations"][0]["base_revision"],
        1
    );
    let old = a
        .database()
        .query_sql("SELECT task_title FROM _sync_server_tasks", vec![])
        .await
        .unwrap();
    assert_eq!(old[0].get::<String>(0).unwrap(), "push first base");
    t.title = "edited after loss".into();
    a.save(&t).await.unwrap();
    calls.lock().unwrap().clear();
    engine.sync_once().await.unwrap();
    assert_eq!(*calls.lock().unwrap(), vec!["push", "pull"]);
    assert_eq!(
        a.database().get::<Task>(t.id).await.unwrap().unwrap().title,
        "edited after loss"
    );
    sync(&a, &server).await;
    assert!(a.conflicts().await.unwrap().is_empty());
}

#[tokio::test]
async fn initial_offline_create_is_pushed_before_snapshot() {
    let server = server().await;
    let a = client().await;
    a.save(&task("initial push")).await.unwrap();
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    SyncEngine::new(
        a,
        TraceTransport {
            server,
            calls: calls.clone(),
            lose: AtomicBool::new(false),
        },
    )
    .sync_once()
    .await
    .unwrap();
    assert_eq!(*calls.lock().unwrap(), vec!["push", "snapshot"]);
}

#[tokio::test]
async fn remote_delete_and_unique_replacement_in_one_page_converge() {
    let server = server().await;
    let a = client().await;
    let old = task("reusable title");
    server.save(&old).await.unwrap();
    sync(&a, &server).await;
    server.delete::<Task>(old.id).await.unwrap();
    let new = task("reusable title");
    assert!(matches!(
        server.save(&new).await.unwrap(),
        PushResult::Accepted(_)
    ));
    sync(&a, &server).await;
    assert!(a.database().get::<Task>(old.id).await.unwrap().is_none());
    assert!(a.database().get::<Task>(new.id).await.unwrap().is_some());
    assert!(a.conflicts().await.unwrap().is_empty());
}

#[tokio::test]
async fn local_atomic_unique_replacement_is_accepted() {
    let server = server().await;
    let a = client().await;
    let old = task("atomic reusable title");
    let old_id = old.id;
    a.save(&old).await.unwrap();
    sync(&a, &server).await;
    let new = task("atomic reusable title");
    let new_id = new.id;
    a.transaction(move |tx| {
        Box::pin(async move {
            tx.delete::<Task>(old_id).await?;
            tx.save(&new).await
        })
    })
    .await
    .unwrap();
    let report = SyncEngine::new(a.clone(), server.clone())
        .sync_once()
        .await
        .unwrap();
    assert_eq!(report.accepted_operations, 1);
    assert_eq!(report.rejected_operations, 0);
    assert!(a.conflicts().await.unwrap().is_empty());
    let snapshot = server.snapshot().await.unwrap();
    assert_eq!(snapshot.cursor.sequence, 2);
    assert!(
        snapshot
            .entities
            .iter()
            .any(|e| e.id == old_id && e.record.is_none())
    );
    assert!(
        snapshot
            .entities
            .iter()
            .any(|e| e.id == new_id && e.record.is_some())
    );
}

#[tokio::test]
async fn fk_move_waits_for_unique_release_before_parent_deletion() {
    let server = server().await;
    let a = client().await;
    let source = task("source parent");
    let occupied = task("target title");
    server.save(&source).await.unwrap();
    server.save(&occupied).await.unwrap();
    let child = OptionalComment {
        id: Uuid::now_v7(),
        task_id: Some(source.id),
        body: "keep child".into(),
    };
    server.save(&child).await.unwrap();
    sync(&a, &server).await;
    let target = task("target title");
    let target_id = target.id;
    let source_id = source.id;
    let occupied_id = occupied.id;
    let child_id = child.id;
    a.transaction(move |tx| {
        Box::pin(async move {
            tx.delete::<Task>(occupied_id).await?;
            tx.save(&target).await?;
            let mut child = tx.get::<OptionalComment>(child_id).await?.unwrap();
            child.task_id = Some(target_id);
            tx.save(&child).await?;
            tx.delete::<Task>(source_id).await
        })
    })
    .await
    .unwrap();
    sync(&a, &server).await;
    assert!(a.conflicts().await.unwrap().is_empty());
    let snapshot = server.snapshot().await.unwrap();
    let child = snapshot.entities.iter().find(|e| e.id == child_id).unwrap();
    assert_eq!(
        child.record.as_ref().unwrap()["task"],
        Value::Uuid(target_id)
    );
    assert_eq!(
        child.record.as_ref().unwrap()["body"],
        Value::Text("keep child".into())
    );
}

struct CreateChildDuringPull {
    server: SyncServer,
    client: SyncClient,
    parent: Uuid,
    child: Uuid,
    optional: bool,
    create: AtomicBool,
}

impl SyncTransport for CreateChildDuringPull {
    fn push(&self, request: PushRequest) -> TransportFuture<'_, PushResult> {
        Box::pin(self.server.push(request))
    }
    fn snapshot(&self) -> TransportFuture<'_, Snapshot> {
        Box::pin(self.server.snapshot())
    }
    fn pull(&self, request: PullRequest) -> TransportFuture<'_, PullResult> {
        Box::pin(async move {
            if self.create.swap(false, Ordering::SeqCst) {
                if self.optional {
                    self.client
                        .save(&OptionalComment {
                            id: self.child,
                            task_id: Some(self.parent),
                            body: "unsent child".into(),
                        })
                        .await?;
                } else {
                    self.client
                        .save(&Comment {
                            id: self.child,
                            task_id: self.parent,
                            body: "unsent child".into(),
                        })
                        .await?;
                }
            }
            self.server.pull(request).await
        })
    }
}

#[tokio::test]
async fn incoming_deletion_preserves_new_children_created_during_pull() {
    for optional in [false, true] {
        let server = server().await;
        let a = client().await;
        let parent = task("parent with unsent child");
        server.save(&parent).await.unwrap();
        sync(&a, &server).await;
        server.delete::<Task>(parent.id).await.unwrap();
        let child_id = Uuid::now_v7();
        let engine = SyncEngine::new(
            a.clone(),
            CreateChildDuringPull {
                server: server.clone(),
                client: a.clone(),
                parent: parent.id,
                child: child_id,
                optional,
                create: AtomicBool::new(true),
            },
        );
        engine.sync_once().await.unwrap();
        if optional {
            let child = a
                .database()
                .get::<OptionalComment>(child_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(child.task_id, Some(parent.id));
            assert_eq!(child.body, "unsent child");
        } else {
            assert_eq!(
                a.database()
                    .get::<Comment>(child_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .body,
                "unsent child"
            );
        }
        let conflicts = a.conflicts().await.unwrap();
        assert!(conflicts.iter().any(|c| c.id == child_id
            && c.local.as_ref().unwrap()["body"] == Value::Text("unsent child".into())));
        // No invented server identity for a never-sent child.
        let rows = a
            .database()
            .query_sql(
                "SELECT entity_id FROM _sync_client_entities WHERE entity_id = $1",
                vec![che_orm::DatabaseValue::Text(child_id.to_string())],
            )
            .await
            .unwrap();
        assert!(rows.is_empty());
        assert!(
            a.resolve(Task::SYNC_NAME, parent.id, Resolution::Remote)
                .await
                .is_err()
        );
        a.resolve(
            if optional {
                OptionalComment::SYNC_NAME
            } else {
                Comment::SYNC_NAME
            },
            child_id,
            Resolution::Remote,
        )
        .await
        .unwrap();
        sync(&a, &server).await;
        assert!(a.database().get::<Task>(parent.id).await.unwrap().is_none());
        assert!(a.conflicts().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn rejected_unsent_child_is_preserved_when_snapshot_deletes_parent() {
    let server = server().await;
    let a = client().await;
    let parent = task("offline cascade");
    server.save(&parent).await.unwrap();
    sync(&a, &server).await;
    let child = Comment {
        id: Uuid::now_v7(),
        task_id: parent.id,
        body: "offline data".into(),
    };
    a.save(&child).await.unwrap();
    server.delete::<Task>(parent.id).await.unwrap();
    sync(&a, &server).await;
    assert_eq!(
        a.database()
            .get::<Comment>(child.id)
            .await
            .unwrap()
            .unwrap()
            .body,
        "offline data"
    );
    assert!(
        a.conflicts()
            .await
            .unwrap()
            .iter()
            .any(|c| c.id == child.id && c.local.is_some())
    );
}

#[tokio::test]
async fn shadow_is_written_before_current_and_failed_application_rolls_both_back() {
    let server = server().await;
    let a = client().await;
    let mut t = task("shadow before current");
    server.save(&t).await.unwrap();
    sync(&a, &server).await;
    let old_cursor = a
        .database()
        .query_sql("SELECT value FROM _sync_meta WHERE key = 'cursor'", vec![])
        .await
        .unwrap()[0]
        .get::<String>(0)
        .unwrap();
    // BEFORE triggers only: assert write order and inject an unexpected failure.
    a.database().execute_sql("CREATE TRIGGER check_shadow BEFORE UPDATE ON tasks WHEN (SELECT task_title FROM _sync_server_tasks WHERE id = NEW.id) != NEW.task_title BEGIN SELECT RAISE(ABORT, 'shadow must precede current'); END", vec![]).await.unwrap();
    a.database().execute_sql("CREATE TRIGGER fail_current BEFORE UPDATE ON tasks BEGIN SELECT RAISE(ABORT, 'temporary application failure'); END", vec![]).await.unwrap();
    t.title = "new authoritative title".into();
    server.save(&t).await.unwrap();
    let engine = SyncEngine::new(a.clone(), server.clone());
    assert!(engine.sync_once().await.is_err());
    assert_eq!(
        a.database().get::<Task>(t.id).await.unwrap().unwrap().title,
        "shadow before current"
    );
    let shadow = a
        .database()
        .query_sql("SELECT task_title FROM _sync_server_tasks", vec![])
        .await
        .unwrap();
    assert_eq!(shadow[0].get::<String>(0).unwrap(), "shadow before current");
    assert_eq!(
        a.database()
            .query_sql("SELECT value FROM _sync_meta WHERE key = 'cursor'", vec![])
            .await
            .unwrap()[0]
            .get::<String>(0)
            .unwrap(),
        old_cursor
    );
    a.database()
        .execute_sql("DROP TRIGGER fail_current", vec![])
        .await
        .unwrap();
    sync(&a, &server).await;
    assert_eq!(
        a.database().get::<Task>(t.id).await.unwrap().unwrap().title,
        "new authoritative title"
    );
    assert!(a.conflicts().await.unwrap().is_empty());
}

#[tokio::test]
async fn explicit_delete_of_deferred_row_survives_reopen_and_is_sent() {
    for transactional in [false, true] {
        let server = server().await;
        let a = client().await;
        let remote = task("deferred deletion");
        server.save(&remote).await.unwrap();
        let mut blocker = task("deferred deletion");
        a.save(&blocker).await.unwrap();
        sync(&a, &server).await;
        assert!(
            a.conflicts()
                .await
                .unwrap()
                .iter()
                .any(|c| c.id == remote.id && c.retryable)
        );
        assert!(a.database().get::<Task>(remote.id).await.unwrap().is_none());
        if transactional {
            let id = remote.id;
            a.transaction(move |tx| Box::pin(async move { tx.delete::<Task>(id).await }))
                .await
                .unwrap();
        } else {
            a.delete::<Task>(remote.id).await.unwrap();
        }
        // Restart the facade before releasing the UNIQUE blocker. The deletion
        // must be durable even though there never was a visible remote row.
        let db = a.database().clone();
        drop(a);
        let a = SyncClient::open(db, registry(), "workspace").await.unwrap();
        blocker.title = "released deletion blocker".into();
        a.save(&blocker).await.unwrap();
        a.resolve(Task::SYNC_NAME, blocker.id, Resolution::Local)
            .await
            .unwrap();
        sync(&a, &server).await;
        assert!(a.database().get::<Task>(remote.id).await.unwrap().is_none());
        assert!(a.conflicts().await.unwrap().is_empty());
        assert!(
            server
                .snapshot()
                .await
                .unwrap()
                .entities
                .iter()
                .any(|e| e.id == remote.id && e.record.is_none())
        );
        // Confirmation consumes the intent rather than retaining stale flags.
        assert!(
            a.database()
                .query_sql(
                    "SELECT key FROM _sync_meta WHERE key LIKE 'local_delete:%'",
                    vec![]
                )
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn rolled_back_delete_of_deferred_row_does_not_cancel_reconciliation() {
    let server = server().await;
    let a = client().await;
    let remote = task("rollback deferred deletion");
    server.save(&remote).await.unwrap();
    let mut blocker = task("rollback deferred deletion");
    a.save(&blocker).await.unwrap();
    sync(&a, &server).await;
    let id = remote.id;
    let result: Result<(), OrmError> = a
        .transaction(move |tx| {
            Box::pin(async move {
                tx.delete::<Task>(id).await?;
                Err(OrmError::Configuration("rollback delete".into()))
            })
        })
        .await;
    assert!(result.is_err());
    assert!(
        a.conflicts()
            .await
            .unwrap()
            .iter()
            .any(|c| c.id == id && c.retryable)
    );
    blocker.title = "released rollback blocker".into();
    a.save(&blocker).await.unwrap();
    a.resolve(Task::SYNC_NAME, blocker.id, Resolution::Local)
        .await
        .unwrap();
    sync(&a, &server).await;
    assert!(a.database().get::<Task>(id).await.unwrap().is_some());
    assert!(a.conflicts().await.unwrap().is_empty());
}

#[tokio::test]
async fn accepted_owned_unique_value_retries_after_older_delete_is_received() {
    let server = server().await;
    let a = client().await;
    let old = OwnedSlot {
        id: Uuid::now_v7(),
        slot: 1,
        title: "old".into(),
        scheduled: None,
    };
    server.save(&old).await.unwrap();
    sync(&a, &server).await;
    server.delete::<OwnedSlot>(old.id).await.unwrap();
    let new = OwnedSlot {
        id: Uuid::now_v7(),
        slot: 2,
        title: "new".into(),
        scheduled: None,
    };
    a.save(&new).await.unwrap();
    let report = SyncEngine::new(a.clone(), server.clone())
        .sync_once()
        .await
        .unwrap();
    assert_eq!(report.accepted_operations, 1);
    assert_eq!(report.conflicts, 0);
    let canonical = a
        .database()
        .get::<OwnedSlot>(new.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(canonical.slot, 1);
    assert_eq!(canonical.title, "new");
    assert!(
        a.database()
            .get::<OwnedSlot>(old.id)
            .await
            .unwrap()
            .is_none()
    );
}

struct NormalizeOwnedAcknowledgement {
    server: SyncServer,
    client: SyncClient,
    edit: AtomicBool,
}

impl SyncTransport for NormalizeOwnedAcknowledgement {
    fn push(&self, mut request: PushRequest) -> TransportFuture<'_, PushResult> {
        Box::pin(async move {
            // Model PostgreSQL's timestamp precision on an in-process SQLite
            // peer, then edit the same writable field while the ACK is in flight.
            for mutation in &mut request.operation.mutations {
                if let Some(Value::DateTime(value)) = mutation
                    .changes
                    .as_mut()
                    .and_then(|r| r.get_mut("scheduled"))
                {
                    *value = che_orm::time::OffsetDateTime::from_unix_timestamp_nanos(
                        value.unix_timestamp_nanos().div_euclid(1000) * 1000,
                    )
                    .unwrap();
                }
            }
            let id = request.operation.mutations[0].id;
            let result = self.server.push(request).await?;
            if self.edit.swap(false, Ordering::SeqCst) {
                let mut slot = self.client.database().get::<OwnedSlot>(id).await?.unwrap();
                slot.scheduled = Some(
                    che_orm::time::OffsetDateTime::from_unix_timestamp_nanos(2_345_678_901)
                        .unwrap(),
                );
                self.client.save(&slot).await?;
            }
            Ok(result)
        })
    }
    fn pull(&self, request: PullRequest) -> TransportFuture<'_, PullResult> {
        Box::pin(self.server.pull(request))
    }
    fn snapshot(&self) -> TransportFuture<'_, Snapshot> {
        Box::pin(self.server.snapshot())
    }
}

#[tokio::test]
async fn deferred_acknowledgement_preserves_in_flight_edits_and_normalization() {
    let server = server().await;
    let a = client().await;
    let old = OwnedSlot {
        id: Uuid::now_v7(),
        slot: 1,
        title: "old normalized slot".into(),
        scheduled: None,
    };
    server.save(&old).await.unwrap();
    sync(&a, &server).await;
    server.delete::<OwnedSlot>(old.id).await.unwrap();
    let new = OwnedSlot {
        id: Uuid::now_v7(),
        slot: 2,
        title: "new normalized slot".into(),
        scheduled: Some(
            che_orm::time::OffsetDateTime::from_unix_timestamp_nanos(1_234_567_890).unwrap(),
        ),
    };
    a.save(&new).await.unwrap();
    let engine = SyncEngine::new(
        a.clone(),
        NormalizeOwnedAcknowledgement {
            server,
            client: a.clone(),
            edit: AtomicBool::new(true),
        },
    );
    let report = engine.sync_once().await.unwrap();
    assert_eq!(report.conflicts, 0);
    let current = a
        .database()
        .get::<OwnedSlot>(new.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.slot, 1);
    assert_eq!(
        current.scheduled.unwrap().unix_timestamp_nanos(),
        2_345_678_901
    );
    for _ in 0..5 {
        if !engine.sync_once().await.unwrap().has_more {
            break;
        }
    }
    assert!(a.conflicts().await.unwrap().is_empty());
    assert_eq!(
        a.database()
            .get::<OwnedSlot>(new.id)
            .await
            .unwrap()
            .unwrap()
            .scheduled
            .unwrap()
            .unix_timestamp_nanos(),
        2_345_678_000
    );
}

#[tokio::test]
async fn newer_remote_revision_is_not_mistaken_for_the_deferred_acknowledgement() {
    struct LagOnePull {
        server: SyncServer,
        lag: AtomicBool,
    }
    impl SyncTransport for LagOnePull {
        fn push(&self, request: PushRequest) -> TransportFuture<'_, PushResult> {
            Box::pin(self.server.push(request))
        }
        fn snapshot(&self) -> TransportFuture<'_, Snapshot> {
            Box::pin(self.server.snapshot())
        }
        fn pull(&self, request: PullRequest) -> TransportFuture<'_, PullResult> {
            Box::pin(async move {
                if self.lag.swap(false, Ordering::SeqCst) {
                    Ok(PullResult::Changes {
                        groups: vec![],
                        cursor: request.cursor,
                        has_more: false,
                    })
                } else {
                    self.server.pull(request).await
                }
            })
        }
    }
    let db = database(SchemaRole::Server).await;
    let server = SyncServer::open(
        db.clone(),
        registry(),
        "workspace",
        "user",
        Arc::new(AllowAll),
    )
    .await
    .unwrap();
    let a = client().await;
    let old = OwnedSlot {
        id: Uuid::now_v7(),
        slot: 1,
        title: "old anchor slot".into(),
        scheduled: None,
    };
    server.save(&old).await.unwrap();
    sync(&a, &server).await;
    server.delete::<OwnedSlot>(old.id).await.unwrap();
    let new = OwnedSlot {
        id: Uuid::now_v7(),
        slot: 2,
        title: "sent anchor title".into(),
        scheduled: None,
    };
    a.save(&new).await.unwrap();
    let engine = SyncEngine::new(
        a.clone(),
        LagOnePull {
            server: server.clone(),
            lag: AtomicBool::new(true),
        },
    );
    engine.sync_once().await.unwrap();
    let deferred = a.conflicts().await.unwrap();
    let conflict = deferred.iter().find(|c| c.id == new.id).unwrap();
    assert!(conflict.retryable);
    assert_eq!(conflict.acknowledgement.as_ref().unwrap().revision, 1);
    let mut local = a
        .database()
        .get::<OwnedSlot>(new.id)
        .await
        .unwrap()
        .unwrap();
    local.title = "local after acknowledgement".into();
    a.save(&local).await.unwrap();
    server
        .save(&OwnedSlot {
            id: new.id,
            slot: 1,
            title: "remote after acknowledgement".into(),
            scheduled: None,
        })
        .await
        .unwrap();
    // A history gap delivers revision 2 directly in a reset snapshot, rather
    // than first replaying revision 1 and clearing its deferred context.
    db.execute_sql("DELETE FROM _sync_changes WHERE sequence = 3", vec![])
        .await
        .unwrap();
    let report = engine.sync_once().await.unwrap();
    assert_eq!(report.conflicts, 1);
    let conflicts = a.conflicts().await.unwrap();
    let conflict = conflicts.iter().find(|c| c.id == new.id).unwrap();
    assert_eq!(conflict.fields, vec!["title"]);
    assert_eq!(
        conflict.base.as_ref().unwrap()["title"],
        Value::Text("sent anchor title".into())
    );
    assert_eq!(conflict.remote.revision, 2);
    assert!(!conflict.retryable);
    assert!(conflict.acknowledgement.is_none());
    let current = a
        .database()
        .get::<OwnedSlot>(new.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.slot, 1);
    assert_eq!(current.title, "local after acknowledgement");
}

#[tokio::test]
async fn dependency_protection_preserves_the_original_rejection_reason() {
    let server = server().await;
    let a = client().await;
    let parent = task("rejected child reason");
    server.save(&parent).await.unwrap();
    sync(&a, &server).await;
    let child = Comment {
        id: Uuid::now_v7(),
        task_id: parent.id,
        body: "unsubmitted child".into(),
    };
    a.save(&child).await.unwrap();
    server.delete::<Task>(parent.id).await.unwrap();
    // Capture the push rejection before delivering the parent's deletion.
    struct CaptureRejection {
        server: SyncServer,
        reason: Arc<std::sync::Mutex<Option<String>>>,
    }
    impl SyncTransport for CaptureRejection {
        fn push(&self, request: PushRequest) -> TransportFuture<'_, PushResult> {
            Box::pin(async move {
                let result = self.server.push(request).await?;
                if let PushResult::Rejected { reason } = &result {
                    *self.reason.lock().unwrap() = Some(reason.clone());
                }
                Ok(result)
            })
        }
        fn pull(&self, request: PullRequest) -> TransportFuture<'_, PullResult> {
            Box::pin(self.server.pull(request))
        }
        fn snapshot(&self) -> TransportFuture<'_, Snapshot> {
            Box::pin(self.server.snapshot())
        }
    }
    let reason = Arc::new(std::sync::Mutex::new(None));
    SyncEngine::new(
        a.clone(),
        CaptureRejection {
            server: server.clone(),
            reason: reason.clone(),
        },
    )
    .sync_once()
    .await
    .unwrap();
    let original_reason = reason.lock().unwrap().clone().unwrap();
    let conflicts = a.conflicts().await.unwrap();
    let conflict = conflicts.iter().find(|c| c.id == child.id).unwrap();
    assert_eq!(conflict.reason.as_deref(), Some(original_reason.as_str()));
    assert!(conflict.fields.is_empty());
    assert!(conflict.base.is_none());
    sync(&a, &server).await;
    assert_eq!(
        a.conflicts()
            .await
            .unwrap()
            .iter()
            .find(|c| c.id == child.id)
            .unwrap()
            .reason
            .as_deref(),
        Some(original_reason.as_str())
    );
}
