#![cfg(feature = "sqlite")]
use che_orm::{
    Database, Migration, MigrationGraph, MigrationId, MigrationOperation, Model, ProjectState,
    SchemaSet, Uuid, ValidatedWrite,
};
use che_sync::{AllowAll, Registry, SyncServer, protocol::model_log::*};
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Model)]
#[orm(table = "journal_tasks", sync, sync_name = "tasks.task")]
struct Task {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(db_column = "caption", sync_name = "title")]
    title: String,
    document: che_orm::JsonField,
    optional_document: Option<che_orm::JsonField>,
    number: i64,
}
#[derive(Debug, Model)]
#[orm(table = "journal_children", sync, sync_name = "tasks.child")]
struct Child {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(foreign_key = Task, on_delete = "cascade")]
    task_id: Uuid,
    label: String,
}
async fn database(client: bool) -> Database {
    let schema = if client {
        SchemaSet::new()
            .client_model::<Task>()
            .client_model::<Child>()
    } else {
        SchemaSet::new()
            .server_model::<Task>()
            .server_model::<Child>()
    };
    let state = ProjectState::from_app_schemas([("tasks", &schema)]).unwrap();
    let operations = state
        .models()
        .iter()
        .cloned()
        .map(|model| MigrationOperation::CreateModel { model })
        .collect();
    let db = Database::connect_in_memory().await.unwrap();
    db.migrate(
        MigrationGraph::new(vec![Migration::new(
            MigrationId {
                app: "tasks".into(),
                name: "0001".into(),
            },
            vec![],
            operations,
        )])
        .unwrap(),
    )
    .await
    .unwrap();
    db
}
fn registry(client: bool) -> Registry {
    Registry::from_schema(&if client {
        SchemaSet::new()
            .client_model::<Task>()
            .client_model::<Child>()
    } else {
        SchemaSet::new()
            .server_model::<Task>()
            .server_model::<Child>()
    })
    .unwrap()
}

#[tokio::test]
async fn client_inspection_distinguishes_empty_initial_pull_and_tracks_dirty_entities() {
    let server = server(database(false).await).await;
    let client = che_sync::ModelLogClient::open(database(true).await, registry(true))
        .await
        .unwrap();
    assert!(!client.has_initial_pull("tasks.task").await.unwrap());
    assert_eq!(client.pending_count().await.unwrap(), 0);
    assert!(client.pending_ids("tasks.task").await.unwrap().is_empty());
    // An empty feed still initializes the projection, even with cursor zero.
    client
        .apply(server.changes::<Task>(0, 100).await.unwrap())
        .await
        .unwrap();
    assert!(client.has_initial_pull("tasks.task").await.unwrap());
    assert!(!client.has_initial_pull("tasks.child").await.unwrap());
    let task = client
        .database()
        .create::<Task>()
        .set(Task::TITLE, "local")
        .set(Task::DOCUMENT, che_orm::JsonField(json!({})))
        .set(Task::OPTIONAL_DOCUMENT, None::<che_orm::JsonField>)
        .set(Task::NUMBER, 1_i64)
        .execute()
        .await
        .unwrap();
    assert_eq!(client.pending_count().await.unwrap(), 1);
    assert_eq!(
        client.pending_ids("tasks.task").await.unwrap(),
        vec![task.id]
    );
    client.database().delete::<Task>(task.id).await.unwrap();
    assert_eq!(
        client.pending_ids("tasks.task").await.unwrap(),
        vec![task.id]
    );
    assert!(client.has_initial_pull("unknown.model").await.is_err());
    assert!(client.pending_ids("unknown.model").await.is_err());
}
async fn server(db: Database) -> SyncServer {
    SyncServer::open_model_logs(db, registry(false), "user", Arc::new(AllowAll))
        .await
        .unwrap()
}
fn insert(id: Uuid, title: &str) -> ValidatedWrite<Task> {
    ValidatedWrite::Create(
        Task::insert()
            .set(Task::ID, id)
            .set(Task::TITLE, title)
            .set(
                Task::DOCUMENT,
                che_orm::JsonField(json!({"a":[1,true,null]})),
            )
            .set(Task::OPTIONAL_DOCUMENT, None::<che_orm::JsonField>)
            .set(Task::NUMBER, i64::MAX),
    )
}

#[tokio::test]
async fn model_journals_are_independent_typed_and_skip_noops() {
    let db = database(false).await;
    let server = server(db.clone()).await;
    let id = Uuid::now_v7();
    server.write(insert(id, "original")).await.unwrap();
    let page = server.changes::<Task>(0, 1).await.unwrap();
    assert_eq!(page.operations.len(), 1);
    assert_eq!(page.operations[0].kind, ChangeKind::Create);
    let field = |name: &str| {
        page.operations[0]
            .diff
            .iter()
            .find(|field| field.field == name)
            .unwrap()
    };
    assert_eq!(
        field("document").new_value,
        Some(json!({"a":[1,true,null]}))
    );
    assert_eq!(field("number").new_value, Some(json!(i64::MAX.to_string())));
    assert_eq!(
        field("optional_document").new_value,
        Some(serde_json::Value::Null)
    );
    assert!(field("optional_document").new_sql_null);
    assert!(field("title").old_value.is_none());
    assert!(
        server
            .changes::<Child>(0, 100)
            .await
            .unwrap()
            .operations
            .is_empty()
    );
    server
        .write(ValidatedWrite::Update(
            Task::update()
                .filter(Task::ID.eq(id))
                .set(Task::TITLE, "original"),
        ))
        .await
        .unwrap();
    assert_eq!(
        server
            .changes::<Task>(0, 100)
            .await
            .unwrap()
            .operations
            .len(),
        1
    );
    server
        .write(ValidatedWrite::Update(
            Task::update()
                .filter(Task::ID.eq(id))
                .set(Task::TITLE, "edited"),
        ))
        .await
        .unwrap();
    let page = server.changes::<Task>(1, 100).await.unwrap();
    assert_eq!(page.operations[0].revision, 2);
    assert_eq!(page.operations[0].diff.len(), 1);
    assert_eq!(
        page.operations[0].diff[0].old_value,
        Some(json!("original"))
    );
    assert_eq!(page.operations[0].diff[0].new_value, Some(json!("edited")));
    assert!(
        db.query_sql("SELECT * FROM _sync_changes", vec![])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn cascades_and_rollbacks_preserve_per_model_history() {
    let db = database(false).await;
    let server = server(db.clone()).await;
    let parent = Uuid::now_v7();
    let child = Uuid::now_v7();
    server.write(insert(parent, "parent")).await.unwrap();
    server
        .write(ValidatedWrite::Create(
            Child::insert()
                .set(Child::ID, child)
                .set(Child::TASK_ID, parent)
                .set(Child::LABEL, "child"),
        ))
        .await
        .unwrap();
    let configured = server.database().clone();
    let result: Result<(), che_orm::OrmError> = configured
        .transaction(move |tx| {
            Box::pin(async move {
                tx.update::<Task>(parent)
                    .set(Task::TITLE, "rollback")
                    .execute()
                    .await?;
                Err(che_orm::OrmError::Configuration("rollback".into()))
            })
        })
        .await;
    assert!(result.is_err());
    assert_eq!(
        server
            .changes::<Task>(0, 100)
            .await
            .unwrap()
            .operations
            .len(),
        1
    );
    server.delete::<Task>(parent).await.unwrap();
    for page in [
        server.changes::<Task>(1, 100).await.unwrap(),
        server.changes::<Child>(1, 100).await.unwrap(),
    ] {
        assert_eq!(page.operations[0].kind, ChangeKind::Delete);
        assert!(
            page.operations[0]
                .diff
                .iter()
                .all(|field| field.new_value.is_none())
        );
    }
}

#[tokio::test]
async fn pagination_uses_model_operation_ids() {
    let server = server(database(false).await).await;
    for n in 0..4 {
        server
            .write(insert(Uuid::now_v7(), &format!("task {n}")))
            .await
            .unwrap();
    }
    let first = server.changes::<Task>(0, 2).await.unwrap();
    assert!(first.has_more);
    assert_eq!(first.next_cursor, 2);
    let second = server.changes::<Task>(first.next_cursor, 2).await.unwrap();
    assert!(!second.has_more);
    assert_eq!(second.next_cursor, 4);
    assert!(server.changes::<Task>(5, 2).await.is_err());
}

#[tokio::test]
async fn missing_parent_defers_projection_but_keeps_latest_state_and_cursor() {
    use che_sync::ModelLogClient;
    let server = server(database(false).await).await;
    let parent = Uuid::now_v7();
    let child = Uuid::now_v7();
    server.write(insert(parent, "parent")).await.unwrap();
    server
        .write(ValidatedWrite::Create(
            Child::insert()
                .set(Child::ID, child)
                .set(Child::TASK_ID, parent)
                .set(Child::LABEL, "first"),
        ))
        .await
        .unwrap();
    let db = database(true).await;
    let client = ModelLogClient::open(db.clone(), registry(true))
        .await
        .unwrap();
    assert!(
        client
            .apply(server.changes::<Child>(0, 100).await.unwrap())
            .await
            .unwrap()
    );
    assert_eq!(client.cursor::<Child>().await.unwrap(), 1);
    assert!(
        client
            .database()
            .get::<Child>(child)
            .await
            .unwrap()
            .is_none()
    );
    let conflicts = client.conflicts().await.unwrap();
    assert_eq!(conflicts.len(), 1);
    assert!(conflicts[0].retryable);
    server
        .write(ValidatedWrite::Update(
            Child::update()
                .filter(Child::ID.eq(child))
                .set(Child::LABEL, "latest"),
        ))
        .await
        .unwrap();
    assert!(
        client
            .apply(server.changes::<Child>(1, 100).await.unwrap())
            .await
            .unwrap()
    );
    assert_eq!(client.cursor::<Child>().await.unwrap(), 2);
    drop(client);
    let client = ModelLogClient::open(db, registry(true)).await.unwrap();
    assert_eq!(client.cursor::<Child>().await.unwrap(), 2);
    assert!(
        client
            .apply(server.changes::<Task>(0, 100).await.unwrap())
            .await
            .unwrap()
    );
    let projected = client
        .database()
        .get::<Child>(child)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(projected.label, "latest");
    assert_eq!(projected.task_id, parent);
    assert!(client.conflicts().await.unwrap().is_empty());
}

#[tokio::test]
async fn invalid_operation_rolls_back_the_entire_page_and_cursor() {
    use che_sync::ModelLogClient;
    let server = server(database(false).await).await;
    let id = Uuid::now_v7();
    server.write(insert(id, "first")).await.unwrap();
    server
        .write(ValidatedWrite::Update(
            Task::update()
                .filter(Task::ID.eq(id))
                .set(Task::TITLE, "second"),
        ))
        .await
        .unwrap();
    let page = server.changes::<Task>(0, 100).await.unwrap();
    let mut invalid = page.clone();
    invalid.operations[1].diff[0].old_value = Some(json!("wrong baseline"));
    let client = ModelLogClient::open(database(true).await, registry(true))
        .await
        .unwrap();
    assert!(client.apply(invalid).await.is_err());
    assert_eq!(client.cursor::<Task>().await.unwrap(), 0);
    assert!(client.database().get::<Task>(id).await.unwrap().is_none());
    assert!(client.conflicts().await.unwrap().is_empty());
    assert!(client.apply(page).await.unwrap());
    assert_eq!(client.cursor::<Task>().await.unwrap(), 2);
    assert_eq!(
        client
            .database()
            .get::<Task>(id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "second"
    );
}

#[tokio::test]
async fn existing_rows_require_explicit_backfill_preserving_revisions() {
    use che_orm::DatabaseValue as D;
    let db = database(false).await;
    let id = Uuid::now_v7();
    insert(id, "existing").save(&db).await.unwrap();
    db.execute_sql(
        "UPDATE journal_tasks SET revision = 7 WHERE id = $1",
        vec![D::Uuid(id)],
    )
    .await
    .unwrap();
    let server = server(db.clone()).await;
    assert!(server.changes::<Task>(0, 100).await.is_err());
    assert!(
        server
            .write(insert(Uuid::now_v7(), "blocked"))
            .await
            .is_err()
    );
    assert!(
        db.query_sql("SELECT id FROM journal_tasks_sync_log", vec![])
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(server.backfill_model_log::<Task>().await.unwrap(), 1);
    let page = server.changes::<Task>(0, 100).await.unwrap();
    assert_eq!(page.operations.len(), 1);
    assert_eq!(page.operations[0].revision, 7);
    assert_eq!(page.operations[0].kind, ChangeKind::Create);
    assert_eq!(page.operations[0].record_id, id);
    assert!(server.backfill_model_log::<Task>().await.is_err());
    server
        .write(ValidatedWrite::Update(
            Task::update()
                .filter(Task::ID.eq(id))
                .set(Task::TITLE, "after baseline"),
        ))
        .await
        .unwrap();
    assert_eq!(
        server.changes::<Task>(1, 100).await.unwrap().operations[0].revision,
        8
    );
}

#[tokio::test]
async fn incompatible_legacy_scope_backfill_rolls_back_every_row() {
    use che_orm::DatabaseValue as D;
    let db = database(false).await;
    let first = Uuid::from_u128(1);
    let second = Uuid::from_u128(2);
    insert(first, "unscoped").save(&db).await.unwrap();
    insert(second, "legacy").save(&db).await.unwrap();
    db.execute_sql(
        "UPDATE journal_tasks SET sync_scope = 'legacy-board', revision = 9 WHERE id = $1",
        vec![D::Uuid(second)],
    )
    .await
    .unwrap();
    let before = db
        .query_sql(
            "SELECT revision, sync_scope, updated_at FROM journal_tasks ORDER BY id",
            vec![],
        )
        .await
        .unwrap();
    let server = server(db.clone()).await;
    assert!(
        server
            .backfill_model_log::<Task>()
            .await
            .unwrap_err()
            .to_string()
            .contains("legacy scope migration")
    );
    let after = db
        .query_sql(
            "SELECT revision, sync_scope, updated_at FROM journal_tasks ORDER BY id",
            vec![],
        )
        .await
        .unwrap();
    for (before, after) in before.iter().zip(after.iter()) {
        assert_eq!(before.get::<i64>(0).unwrap(), after.get::<i64>(0).unwrap());
        assert_eq!(
            before.get::<String>(1).unwrap(),
            after.get::<String>(1).unwrap()
        );
        assert_eq!(
            before.get::<che_orm::time::OffsetDateTime>(2).unwrap(),
            after.get::<che_orm::time::OffsetDateTime>(2).unwrap()
        );
    }
    assert!(
        db.query_sql("SELECT id FROM journal_tasks_sync_log", vec![])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(server.changes::<Task>(0, 100).await.is_err());
}

#[tokio::test]
async fn opening_model_client_over_legacy_database_does_not_modify_metadata() {
    use che_sync::{ModelLogClient, SyncClient};
    let db = database(true).await;
    let _legacy = SyncClient::open(db.clone(), registry(true), "legacy-board")
        .await
        .unwrap();
    let metadata = || async {
        db.query_sql("SELECT key, value FROM _sync_meta ORDER BY key", vec![])
            .await
            .unwrap()
            .iter()
            .map(|row| (row.get::<String>(0).unwrap(), row.get::<String>(1).unwrap()))
            .collect::<Vec<_>>()
    };
    let before = metadata().await;
    assert!(
        ModelLogClient::open(db.clone(), registry(true))
            .await
            .is_err()
    );
    assert_eq!(metadata().await, before);
}

#[tokio::test]
async fn failed_protocol_switch_leaves_legacy_request_settleable() {
    use che_sync::{
        ModelLogClient, SyncClient, SyncEngine, SyncTransport, TransportFuture, protocol::*,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    struct LostReply {
        server: SyncServer,
        lose: AtomicBool,
    }
    impl SyncTransport for LostReply {
        fn push(&self, request: PushRequest) -> TransportFuture<'_, PushResult> {
            Box::pin(async move {
                let result = self.server.push(request).await?;
                if self.lose.swap(false, Ordering::SeqCst) {
                    Err(che_orm::OrmError::Interaction("lost reply".into()))
                } else {
                    Ok(result)
                }
            })
        }
        fn pull(&self, request: PullRequest) -> TransportFuture<'_, PullResult> {
            Box::pin(self.server.pull(request))
        }
        fn snapshot(&self) -> TransportFuture<'_, Snapshot> {
            Box::pin(self.server.snapshot())
        }
    }
    let legacy = SyncServer::open(
        database(false).await,
        registry(false),
        "legacy-board",
        "user",
        Arc::new(AllowAll),
    )
    .await
    .unwrap();
    let db = database(true).await;
    let client = SyncClient::open(db.clone(), registry(true), "legacy-board")
        .await
        .unwrap();
    let engine = SyncEngine::new(
        client.clone(),
        LostReply {
            server: legacy,
            lose: AtomicBool::new(true),
        },
    );
    engine.full_load().await.unwrap();
    let id = Uuid::now_v7();
    client
        .save(&Task {
            id,
            title: "pending".into(),
            document: che_orm::JsonField(json!({})),
            optional_document: None,
            number: i64::MAX,
        })
        .await
        .unwrap();
    assert!(engine.flush().await.is_err());
    let pending = db
        .query_sql("SELECT payload FROM _sync_outbox", vec![])
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    let payload = pending[0].get::<String>(0).unwrap();
    assert!(
        ModelLogClient::open(db.clone(), registry(true))
            .await
            .is_err()
    );
    let pending = db
        .query_sql("SELECT payload FROM _sync_outbox", vec![])
        .await
        .unwrap();
    assert_eq!(pending[0].get::<String>(0).unwrap(), payload);
    assert_eq!(engine.flush().await.unwrap().accepted_operations, 1);
    assert!(
        db.query_sql("SELECT payload FROM _sync_outbox", vec![])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        db.query_sql(
            "SELECT value FROM _sync_meta WHERE key = 'model_log_protocol'",
            vec![]
        )
        .await
        .unwrap()
        .is_empty()
    );
}

#[tokio::test]
async fn model_protocol_ownership_and_failed_initialization_are_atomic() {
    use che_orm::DatabaseValue as D;
    use che_sync::{ModelLogClient, SyncClient};
    let db = database(true).await;
    // Replica decoding fails after the scope/protocol initialization attempts.
    db.execute_sql(
        "INSERT INTO _sync_meta (key, value) VALUES ('replica_id', $1)",
        vec![D::Text("invalid JSON".into())],
    )
    .await
    .unwrap();
    assert!(
        ModelLogClient::open(db.clone(), registry(true))
            .await
            .is_err()
    );
    let rows = db
        .query_sql("SELECT key FROM _sync_meta ORDER BY key", vec![])
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<String>(0).unwrap(), "replica_id");
    db.execute_sql("DELETE FROM _sync_meta", vec![])
        .await
        .unwrap();
    let _client = ModelLogClient::open(db.clone(), registry(true))
        .await
        .unwrap();
    // Even the same internal scope cannot reinterpret v2 state as legacy.
    assert!(
        SyncClient::open(
            db.clone(),
            registry(true),
            che_sync::model_log::MODEL_LOG_SCOPE
        )
        .await
        .is_err()
    );
    assert!(ModelLogClient::open(db, registry(true)).await.is_ok());
}

#[tokio::test]
async fn journal_gaps_are_applied_without_assuming_contiguous_ids() {
    use che_sync::ModelLogClient;
    let db = database(false).await;
    let server = server(db.clone()).await;
    let id = Uuid::now_v7();
    server.write(insert(id, "first")).await.unwrap();
    // A cursor is an ordered position, not a count of journal rows.
    db.execute_sql(
        "UPDATE journal_tasks_sync_log_state SET sequence = sequence + 4 WHERE key = 0",
        vec![],
    )
    .await
    .unwrap();
    server
        .write(ValidatedWrite::Update(
            Task::update()
                .filter(Task::ID.eq(id))
                .set(Task::TITLE, "after gap"),
        ))
        .await
        .unwrap();
    let client = ModelLogClient::open(database(true).await, registry(true))
        .await
        .unwrap();
    let first = server.changes::<Task>(0, 1).await.unwrap();
    assert!(first.has_more);
    assert_eq!(first.next_cursor, 1);
    client.apply(first).await.unwrap();
    let second = server
        .changes::<Task>(client.cursor::<Task>().await.unwrap(), 1)
        .await
        .unwrap();
    assert_eq!(second.next_cursor, 6);
    assert!(!second.has_more);
    client.apply(second).await.unwrap();
    assert_eq!(client.cursor::<Task>().await.unwrap(), 6);
    assert_eq!(
        client
            .database()
            .get::<Task>(id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "after gap"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writers_and_paginated_reader_do_not_skip_commits() {
    let server = server(database(false).await).await;
    let mut writers = Vec::new();
    for writer in 0..2 {
        let server = server.clone();
        writers.push(tokio::spawn(async move {
            let mut ids = Vec::new();
            for n in 0..20 {
                let id = Uuid::now_v7();
                server
                    .write(insert(id, &format!("writer {writer}: {n}")))
                    .await
                    .unwrap();
                ids.push(id);
                tokio::task::yield_now().await;
            }
            ids
        }));
    }
    let mut after = 0;
    let mut downloaded = std::collections::BTreeSet::new();
    loop {
        // Check completion before the page so the final read includes all
        // commits. Joining below also surfaces writer failures without spinning.
        let finished = writers.iter().all(tokio::task::JoinHandle::is_finished);
        let page = server.changes::<Task>(after, 3).await.unwrap();
        for operation in &page.operations {
            assert!(operation.id > after);
            assert!(downloaded.insert(operation.record_id));
        }
        after = page.next_cursor;
        if finished && !page.has_more {
            break;
        }
        tokio::task::yield_now().await;
    }
    let mut committed = std::collections::BTreeSet::new();
    for writer in writers {
        committed.extend(writer.await.unwrap());
    }
    assert_eq!(downloaded, committed);
    assert_eq!(downloaded.len(), 40);
    assert_eq!(after, 40);
}

#[tokio::test]
async fn independent_fields_merge_and_overlapping_or_delete_edits_require_resolution() {
    use che_sync::{ModelLogClient, Resolution};
    let server = server(database(false).await).await;
    let id = Uuid::now_v7();
    server.write(insert(id, "base")).await.unwrap();
    let client = ModelLogClient::open(database(true).await, registry(true))
        .await
        .unwrap();
    client
        .apply(server.changes::<Task>(0, 100).await.unwrap())
        .await
        .unwrap();
    let mut local = client.database().get::<Task>(id).await.unwrap().unwrap();
    local.title = "local edit".into();
    client.save(&local).await.unwrap();
    server
        .write(ValidatedWrite::Update(
            Task::update().filter(Task::ID.eq(id)).set(Task::NUMBER, 42),
        ))
        .await
        .unwrap();
    client
        .apply(server.changes::<Task>(1, 100).await.unwrap())
        .await
        .unwrap();
    let merged = client.database().get::<Task>(id).await.unwrap().unwrap();
    assert_eq!(merged.title, "local edit");
    assert_eq!(merged.number, 42);
    assert!(client.conflicts().await.unwrap().is_empty());
    server
        .write(ValidatedWrite::Update(
            Task::update()
                .filter(Task::ID.eq(id))
                .set(Task::TITLE, "remote edit"),
        ))
        .await
        .unwrap();
    client
        .apply(server.changes::<Task>(2, 100).await.unwrap())
        .await
        .unwrap();
    let conflicts = client.conflicts().await.unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].fields, vec!["title"]);
    assert!(!conflicts[0].retryable);
    assert_eq!(
        client
            .database()
            .get::<Task>(id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "local edit"
    );
    client
        .resolve("tasks.task", id, Resolution::Remote)
        .await
        .unwrap();
    assert_eq!(
        client
            .database()
            .get::<Task>(id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "remote edit"
    );
    assert!(client.conflicts().await.unwrap().is_empty());
    client.delete::<Task>(id).await.unwrap();
    server
        .write(ValidatedWrite::Update(
            Task::update()
                .filter(Task::ID.eq(id))
                .set(Task::TITLE, "updated after local deletion"),
        ))
        .await
        .unwrap();
    client
        .apply(server.changes::<Task>(3, 100).await.unwrap())
        .await
        .unwrap();
    assert!(client.database().get::<Task>(id).await.unwrap().is_none());
    let conflicts = client.conflicts().await.unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].fields, vec!["$entity"]);
    assert!(conflicts[0].remote.record.is_some());
    client
        .resolve("tasks.task", id, Resolution::Remote)
        .await
        .unwrap();
    assert_eq!(
        client
            .database()
            .get::<Task>(id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "updated after local deletion"
    );
    assert!(client.conflicts().await.unwrap().is_empty());
}

#[derive(Debug, Model)]
#[orm(table = "typed_events", sync, sync_name = "events.event")]
struct TypedEvent {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(db_column = "occurred", sync_name = "timestamp")]
    happened_at: che_orm::time::OffsetDateTime,
    number: i64,
    document: Option<che_orm::JsonField>,
}

#[tokio::test]
async fn typed_journal_round_trips_dates_json_scalars_sql_null_and_integer_extremes() {
    use che_sync::ModelLogClient;
    async fn setup(client: bool) -> (Database, Registry) {
        let schema = if client {
            SchemaSet::new().client_model::<TypedEvent>()
        } else {
            SchemaSet::new().server_model::<TypedEvent>()
        };
        let registry = Registry::from_schema(&schema).unwrap();
        let state = ProjectState::from_app_schemas([("events", &schema)]).unwrap();
        let db = Database::connect_in_memory().await.unwrap();
        db.migrate(
            MigrationGraph::new(vec![Migration::new(
                MigrationId {
                    app: "events".into(),
                    name: "0001".into(),
                },
                vec![],
                state
                    .models()
                    .iter()
                    .cloned()
                    .map(|model| MigrationOperation::CreateModel { model })
                    .collect(),
            )])
            .unwrap(),
        )
        .await
        .unwrap();
        (db, registry)
    }
    let (db, registry) = setup(false).await;
    let server = SyncServer::open_model_logs(db, registry, "user", Arc::new(AllowAll))
        .await
        .unwrap();
    let (db, registry) = setup(true).await;
    let client = ModelLogClient::open(db, registry).await.unwrap();
    let id = Uuid::now_v7();
    let timestamp = che_orm::time::OffsetDateTime::parse(
        "2026-10-20T08:09:10.123456789+05:00",
        &che_orm::time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    server
        .write(ValidatedWrite::Create(
            TypedEvent::insert()
                .set(TypedEvent::ID, id)
                .set(TypedEvent::HAPPENED_AT, timestamp)
                .set(TypedEvent::NUMBER, i64::MIN)
                .set(TypedEvent::DOCUMENT, None::<che_orm::JsonField>),
        ))
        .await
        .unwrap();
    client
        .apply(server.changes::<TypedEvent>(0, 100).await.unwrap())
        .await
        .unwrap();
    let saved = client
        .database()
        .get::<TypedEvent>(id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.happened_at, timestamp);
    assert_eq!(saved.number, i64::MIN);
    assert_eq!(saved.document, None);
    for (index, value) in [
        json!(null),
        json!({"k":[1,true,null]}),
        json!([false, "x"]),
        json!(true),
        json!("text"),
        json!(42),
    ]
    .into_iter()
    .enumerate()
    {
        let after = client.cursor::<TypedEvent>().await.unwrap();
        server
            .write(ValidatedWrite::Update(
                TypedEvent::update()
                    .filter(TypedEvent::ID.eq(id))
                    .set(TypedEvent::NUMBER, i64::MAX)
                    .set(
                        TypedEvent::DOCUMENT,
                        Some(che_orm::JsonField(value.clone())),
                    ),
            ))
            .await
            .unwrap();
        let page = server.changes::<TypedEvent>(after, 100).await.unwrap();
        assert_eq!(page.operations.len(), 1);
        let document = page.operations[0]
            .diff
            .iter()
            .find(|diff| diff.field == "document")
            .unwrap();
        assert_eq!(document.new_value, Some(value.clone()));
        assert!(!document.new_sql_null);
        if index == 0 {
            assert!(document.old_sql_null);
        }
        client.apply(page).await.unwrap();
        let saved = client
            .database()
            .get::<TypedEvent>(id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.document, Some(che_orm::JsonField(value)));
        assert_eq!(saved.number, i64::MAX);
        assert_eq!(saved.happened_at, timestamp);
    }
}

#[tokio::test]
async fn native_delivery_persists_opaque_payloads_and_rejects_another_transport() {
    opaque_delivery_scenario(false).await;
}
#[tokio::test]
async fn unclaimed_legacy_payload_cannot_be_reencoded_by_another_transport() {
    opaque_delivery_scenario(true).await;
}
async fn opaque_delivery_scenario(legacy: bool) {
    use che_sync::{ModelLogClient, ModelLogTransport, TransportFuture};
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    struct Native {
        id: &'static str,
        codec: &'static str,
        fail: bool,
        prepared: Arc<AtomicUsize>,
        delivered: Arc<Mutex<Vec<serde_json::Value>>>,
    }
    impl ModelLogTransport for Native {
        fn id(&self) -> &'static str {
            self.id
        }
        fn legacy_payload_key(&self, operation: Uuid) -> Option<String> {
            (self.id == "native.v1").then(|| format!("old-native:{operation}"))
        }
        fn prepare_write(
            &self,
            write: &ModelWrite,
        ) -> Result<serde_json::Value, che_orm::OrmError> {
            self.prepared.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"codec":self.codec, "mutation":write}))
        }
        fn write<'a>(
            &'a self,
            write: &'a ModelWrite,
            payload: &'a serde_json::Value,
        ) -> TransportFuture<'a, ModelWriteResult> {
            Box::pin(async move {
                assert_eq!(payload["mutation"], serde_json::to_value(write).unwrap());
                self.delivered.lock().unwrap().push(payload.clone());
                if self.fail {
                    Err(che_orm::OrmError::Interaction(
                        "delivery interrupted".into(),
                    ))
                } else {
                    Ok(ModelWriteResult::Rejected {
                        code: "domain".into(),
                        reason: "explicit domain rejection".into(),
                    })
                }
            })
        }
        fn changes<'a>(&'a self, _: &'a str, _: i64, _: u32) -> TransportFuture<'a, ModelChanges> {
            Box::pin(async { panic!("download not needed") })
        }
    }
    let db = database(true).await;
    let client = ModelLogClient::open(db.clone(), registry(true))
        .await
        .unwrap();
    client
        .apply(ModelChanges {
            model: "tasks.task".into(),
            history: Uuid::now_v7(),
            operations: vec![],
            next_cursor: 0,
            has_more: false,
        })
        .await
        .unwrap();
    let prepared = Arc::new(AtomicUsize::new(0));
    let delivered = Arc::new(Mutex::new(vec![]));
    let transport = |id, codec, fail| Native {
        id,
        codec,
        fail,
        prepared: prepared.clone(),
        delivered: delivered.clone(),
    };
    let id = Uuid::now_v7();
    client
        .save(&Task {
            id,
            title: "submitted".into(),
            document: che_orm::JsonField(json!({})),
            optional_document: None,
            number: i64::MIN,
        })
        .await
        .unwrap();
    let engine = client.engine(transport("native.v1", "original", true));
    assert!(engine.flush().await.is_err());
    let stored = db
        .query_sql(
            "SELECT value FROM _sync_meta WHERE key LIKE 'submitted-write:%'",
            vec![],
        )
        .await
        .unwrap()[0]
        .get::<String>(0)
        .unwrap();
    let submitted: serde_json::Value = serde_json::from_str(&stored).unwrap();
    let operation = submitted["payload"]["mutation"]["operation_id"]
        .as_str()
        .unwrap();
    let stored_key = if legacy {
        format!("old-native:{operation}")
    } else {
        format!("submitted-write:{operation}")
    };
    let stored = if legacy {
        let payload = serde_json::to_string(&submitted["payload"]).unwrap();
        db.execute_sql(
            "DELETE FROM _sync_meta WHERE key LIKE 'submitted-write:%'",
            vec![],
        )
        .await
        .unwrap();
        db.execute_sql(
            "INSERT INTO _sync_meta (key, value) VALUES ($1,$2)",
            vec![
                che_orm::DatabaseValue::Text(stored_key.clone()),
                che_orm::DatabaseValue::Text(payload.clone()),
            ],
        )
        .await
        .unwrap();
        payload
    } else {
        stored
    };
    drop(engine);
    drop(client);
    let client = ModelLogClient::open(db.clone(), registry(true))
        .await
        .unwrap();
    let wrong = client.engine(transport("other.v1", "changed", false));
    assert!(
        wrong
            .flush()
            .await
            .unwrap_err()
            .to_string()
            .contains("another transport")
    );
    assert_eq!(prepared.load(Ordering::SeqCst), 1);
    assert_eq!(delivered.lock().unwrap().len(), 1);
    assert_eq!(
        db.query_sql(
            "SELECT value FROM _sync_meta WHERE key = $1",
            vec![che_orm::DatabaseValue::Text(stored_key.clone())]
        )
        .await
        .unwrap()[0]
            .get::<String>(0)
            .unwrap(),
        stored
    );
    drop(wrong);
    let engine = client.engine(transport("native.v1", "changed", false));
    assert_eq!(engine.flush().await.unwrap().rejected_operations, 1);
    assert_eq!(
        prepared.load(Ordering::SeqCst),
        1,
        "submitted payload was re-encoded"
    );
    {
        let delivered = delivered.lock().unwrap();
        assert_eq!(delivered.len(), 2);
        assert_eq!(delivered[0], delivered[1]);
    }
    assert!(
        db.query_sql("SELECT operation_id FROM _sync_outbox", vec![])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        db.query_sql(
            "SELECT value FROM _sync_meta WHERE key LIKE 'submitted-write:%'",
            vec![]
        )
        .await
        .unwrap()
        .is_empty()
    );
}
