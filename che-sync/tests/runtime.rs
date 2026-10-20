use che_orm::{
    Database, Migration, MigrationGraph, MigrationId, MigrationOperation, Model, OrmError,
    TableState, Uuid,
};
use che_sync::{
    AllowAll, Field, Registry, SchemaRole, SyncClient, SyncEngine, SyncModel, SyncServer,
    protocol::*,
};
use std::sync::Arc;

#[derive(Debug, Model)]
#[orm(table = "sync_notes")]
struct Note {
    #[orm(primary_key)]
    id: Uuid,
    title: String,
    enabled: bool,
    #[orm(auto_now)]
    updated: che_orm::time::OffsetDateTime,
    #[orm(default = "0")]
    revision: i64,
    #[orm(auto_now)]
    updated_at: che_orm::time::OffsetDateTime,
    deleted_at: Option<che_orm::time::OffsetDateTime>,
    #[orm(default = "''")]
    sync_scope: String,
}

impl che_sync::SyncModel for Note {
    const SYNC_NAME: &'static str = "notes.note";
    fn sync_fields() -> Vec<Field> {
        vec![
            Field::server_owned("id", "id"),
            Field::writable("title", "title"),
            Field::writable("enabled", "enabled"),
            Field::server_owned("updated", "updated"),
        ]
    }
}

#[derive(Debug, Model)]
#[orm(table = "sync_children")]
struct Child {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(foreign_key = Note, on_delete = "cascade")]
    note_id: Uuid,
    body: String,
    #[orm(default = "0")]
    revision: i64,
    #[orm(auto_now)]
    updated_at: che_orm::time::OffsetDateTime,
    deleted_at: Option<che_orm::time::OffsetDateTime>,
    #[orm(default = "''")]
    sync_scope: String,
}

impl SyncModel for Child {
    const SYNC_NAME: &'static str = "notes.child";
    fn sync_fields() -> Vec<Field> {
        vec![
            Field::server_owned("id", "id"),
            Field::writable("note", "note_id"),
            Field::writable("body", "body"),
        ]
    }
}

fn request(title: &str, replica: Uuid) -> PushRequest {
    PushRequest {
        protocol_version: 1,
        scope: "scope".into(),
        replica_id: replica,
        operation: Operation {
            id: Uuid::now_v7(),
            mutations: vec![Mutation {
                model: "notes.note".into(),
                id: Uuid::now_v7(),
                schema_version: 1,
                base_revision: 0,
                changes: Some(Record::from([
                    ("title".into(), Value::Text(title.into())),
                    ("enabled".into(), Value::Boolean(true)),
                ])),
            }],
        },
    }
}

#[tokio::test]
#[cfg_attr(
    feature = "postgres",
    ignore = "requires TEST_POSTGRES_URL and a live PostgreSQL server"
)]
async fn portable_server_and_client_synchronization() {
    let (db, cleanup) = database().await;
    let (first, first_cleanup) = database().await;
    let (second, second_cleanup) = database().await;
    let result = scenario(db, first, second).await;
    for cleanup in [cleanup, first_cleanup, second_cleanup]
        .into_iter()
        .flatten()
    {
        cleanup
            .0
            .execute_sql(format!("DROP SCHEMA \"{}\" CASCADE", cleanup.1), vec![])
            .await
            .unwrap();
    }
    result.unwrap();
}

#[cfg(feature = "sqlite")]
async fn database() -> (Database, Option<(Database, String)>) {
    (Database::connect_in_memory().await.unwrap(), None)
}

#[cfg(feature = "postgres")]
async fn database() -> (Database, Option<(Database, String)>) {
    let url = std::env::var("TEST_POSTGRES_URL")
        .expect("set TEST_POSTGRES_URL to run PostgreSQL sync tests");
    let admin = Database::connect(url.clone()).await.unwrap();
    let schema = format!("sync_test_{}", Uuid::now_v7().simple());
    admin
        .execute_sql(format!("CREATE SCHEMA \"{schema}\""), vec![])
        .await
        .unwrap();
    let separator = if url.contains('?') { '&' } else { '?' };
    let db = Database::connect(format!("{url}{separator}options=-csearch_path%3D{schema}"))
        .await
        .unwrap();
    (db, Some((admin, schema)))
}

async fn migrate(
    db: &Database,
    registry: &Registry,
    role: SchemaRole,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut ops = vec![
        MigrationOperation::CreateTable {
            table: TableState::from(&Note::schema()),
        },
        MigrationOperation::CreateTable {
            table: TableState::from(&Child::schema()),
        },
    ];
    ops.extend(registry.migration_operations(role));
    ops.push(MigrationOperation::RunSql { forward: "CREATE UNIQUE INDEX sync_notes_active_title ON sync_notes(title) WHERE deleted_at IS NULL".into(), reverse: None, state_operations: vec![] });
    db.migrate(MigrationGraph::new(vec![Migration::new(
        MigrationId {
            app: "sync".into(),
            name: "0001".into(),
        },
        vec![],
        ops,
    )])?)
    .await?;
    Ok(())
}

async fn scenario(
    db: Database,
    first: Database,
    second: Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let registry = Registry::new().register::<Note>()?.register::<Child>()?;
    migrate(&db, &registry, SchemaRole::Server).await?;
    migrate(&first, &registry, SchemaRole::Client).await?;
    migrate(&second, &registry, SchemaRole::Client).await?;
    let a = SyncClient::open(first, registry.clone(), "scope").await?;
    let b = SyncClient::open(second, registry.clone(), "scope").await?;
    let server =
        SyncServer::open(db, registry, "scope", "trusted-user", Arc::new(AllowAll)).await?;
    let replica = Uuid::now_v7();
    let first = request("first", replica);
    let result = server.push(first.clone()).await?;
    if !matches!(result, PushResult::Accepted(_)) {
        return Err("first push rejected".into());
    }
    if result != server.push(first).await? {
        return Err("receipt replay changed result".into());
    }
    let PushResult::Accepted(ref entities) = result else {
        unreachable!()
    };
    let updated = server
        .write(che_orm::ValidatedWrite::Update(
            Note::update()
                .filter(Note::ID.eq(entities[0].id))
                .set(Note::ENABLED, false),
        ))
        .await?;
    if !matches!(updated, PushResult::Accepted(ref rows)
        if rows[0].revision == 2 && rows[0].record.as_ref().is_some_and(|record|
            record["enabled"] == Value::Boolean(false) && matches!(record["updated"], Value::DateTime(_))))
    {
        return Err("validated write did not advance managed state".into());
    }
    let snapshot = server.snapshot().await?;
    let model_server = server.for_model::<Note>()?;
    let model_snapshot = model_server.snapshot().await?;
    if model_snapshot.cursor.model.as_deref() != Some(Note::SYNC_NAME)
        || model_snapshot.entities != snapshot.entities
    {
        return Err("invalid model snapshot".into());
    }
    if model_server
        .pull(PullRequest {
            protocol_version: PROTOCOL_VERSION,
            cursor: Some(snapshot.cursor.clone()),
            limit: 100,
        })
        .await
        .is_ok()
    {
        return Err("model feed accepted a whole-scope cursor".into());
    }
    let cursor = snapshot.cursor;
    // Concurrent writers must expose a contiguous committed journal.
    let mut handles = Vec::new();
    for n in 0..12 {
        let server = server.clone();
        handles.push(tokio::spawn(async move {
            server.push(request(&format!("note-{n}"), replica)).await
        }));
    }
    for handle in handles {
        if !matches!(handle.await??, PushResult::Accepted(_)) {
            return Err("concurrent push rejected".into());
        }
    }
    match server
        .pull(PullRequest {
            protocol_version: 1,
            cursor: Some(cursor.clone()),
            limit: 100,
        })
        .await?
    {
        PullResult::Changes {
            groups,
            cursor: next,
            has_more: false,
        } if groups.len() == 12 && next.sequence == cursor.sequence + 12 => {
            for (index, group) in groups.iter().enumerate() {
                if group.sequence != cursor.sequence + index as i64 + 1 {
                    return Err("journal gap".into());
                }
            }
        }
        _ => return Err("unexpected pull page".into()),
    }
    let before = server.snapshot().await?;
    let mut rejected = request("duplicate", replica);
    let mut duplicate = rejected.operation.mutations[0].clone();
    duplicate.id = Uuid::now_v7();
    rejected.operation.mutations.push(duplicate);
    if !matches!(server.push(rejected).await?, PushResult::Rejected { .. }) {
        return Err("constraint group accepted".into());
    }
    if server.snapshot().await? != before {
        return Err(OrmError::Interaction("rejected group changed snapshot".into()).into());
    }
    // A failed first INSERT must release its savepoint, allow the old row's
    // deletion, and then retry successfully without aborting PostgreSQL's tx.
    let old = before
        .entities
        .iter()
        .find(|e| {
            e.record
                .as_ref()
                .is_some_and(|r| r["title"] == Value::Text("first".into()))
        })
        .unwrap();
    let mut replacement = request("first", replica);
    let replacement_id = replacement.operation.mutations[0].id;
    replacement.operation.mutations.push(Mutation {
        model: old.model.clone(),
        id: old.id,
        schema_version: old.schema_version,
        base_revision: old.revision,
        changes: None,
    });
    let result = server.push(replacement.clone()).await?;
    if !matches!(result, PushResult::Accepted(_)) {
        return Err("valid UNIQUE replacement rejected".into());
    }
    if result != server.push(replacement).await? {
        return Err("replacement receipt changed".into());
    }
    let after = server.snapshot().await?;
    if after.cursor.sequence != before.cursor.sequence + 1
        || !after
            .entities
            .iter()
            .any(|e| e.id == old.id && e.record.is_none())
        || !after
            .entities
            .iter()
            .any(|e| e.id == replacement_id && e.record.is_some())
    {
        return Err("replacement group was not atomic".into());
    }
    let note = Note {
        id: Uuid::now_v7(),
        title: "offline create".into(),
        enabled: false,
        updated: che_orm::time::OffsetDateTime::UNIX_EPOCH,
        revision: 0,
        updated_at: che_orm::time::OffsetDateTime::UNIX_EPOCH,
        deleted_at: None,
        sync_scope: String::new(),
    };
    a.save(&note).await?;
    drain(&a, &server).await?;
    drain(&b, &server).await?;
    let mut left = a.database().get::<Note>(note.id).await?.unwrap();
    let mut right = b.database().get::<Note>(note.id).await?.unwrap();
    assert_eq!(left.revision, 1);
    left.title = "offline title edit".into();
    right.enabled = true;
    a.save(&left).await?;
    b.save(&right).await?;
    drain(&a, &server).await?;
    drain(&b, &server).await?;
    drain(&a, &server).await?;
    for client in [&a, &b] {
        let merged = client.database().get::<Note>(note.id).await?.unwrap();
        assert_eq!(merged.title, "offline title edit");
        assert!(merged.enabled);
        assert_eq!(merged.revision, 3);
        assert!(client.conflicts().await?.is_empty());
    }
    a.delete::<Note>(note.id).await?;
    drain(&a, &server).await?;
    drain(&b, &server).await?;
    assert!(b.database().get::<Note>(note.id).await?.is_none());
    let tombstone = server
        .snapshot()
        .await?
        .entities
        .into_iter()
        .find(|e| e.id == note.id)
        .unwrap();
    assert_eq!(tombstone.revision, 4);
    assert!(tombstone.record.is_none());
    assert!(tombstone.deleted_at.is_some());
    let parent = Note {
        id: Uuid::now_v7(),
        title: "automatic parent".into(),
        ..note
    };
    let child = Child {
        id: Uuid::now_v7(),
        note_id: parent.id,
        body: "offline child".into(),
        revision: 0,
        updated_at: che_orm::time::OffsetDateTime::UNIX_EPOCH,
        deleted_at: None,
        sync_scope: String::new(),
    };
    a.save(&parent).await?;
    a.save(&child).await?;
    drain_model::<Child>(&a, &server).await?;
    drain_model::<Child>(&b, &server).await?;
    assert_eq!(
        b.database().get::<Child>(child.id).await?.unwrap().note_id,
        parent.id
    );
    assert_eq!(
        b.database().get::<Note>(parent.id).await?.unwrap().revision,
        1
    );
    let before_delete = server.snapshot().await?.cursor.sequence;
    let PushResult::Accepted(deleted) = server.delete::<Note>(parent.id).await? else {
        panic!("cascade rejected");
    };
    assert_eq!(deleted.len(), 2);
    assert!(
        deleted
            .iter()
            .all(|entity| entity.record.is_none() && entity.deleted_at.is_some())
    );
    assert_eq!(server.snapshot().await?.cursor.sequence, before_delete + 1);
    drain_model::<Child>(&b, &server).await?;
    assert!(b.database().get::<Child>(child.id).await?.is_none());
    assert!(b.database().get::<Note>(parent.id).await?.is_none());
    let mut rest = server
        .prepare_write(che_orm::ValidatedWrite::Create(
            Note::insert()
                .set(Note::TITLE, "REST receipt")
                .set(Note::ENABLED, true),
        ))
        .await?;
    rest.replica_id = Uuid::now_v7();
    let identity = "POST /notes/ with immutable serializer JSON".to_owned();
    let before = server.prepare_push(&rest).await?;
    let committed = server
        .write_rest_prepared(rest.clone(), before, vec![], identity.clone())
        .await?;
    assert!(matches!(committed, PushResult::Accepted(_)));
    let retry = server
        .prepare_rest_write(
            che_orm::ValidatedWrite::Create(
                Note::insert()
                    .set(Note::ID, rest.operation.mutations[0].id)
                    .set(Note::TITLE, "REST receipt")
                    .set(Note::ENABLED, true),
            ),
            0,
        )
        .await?;
    assert_eq!(retry.operation.mutations, rest.operation.mutations);
    assert!(
        server
            .prepare_write(che_orm::ValidatedWrite::Create(
                Note::insert()
                    .set(Note::ID, rest.operation.mutations[0].id)
                    .set(Note::TITLE, "REST receipt")
                    .set(Note::ENABLED, true),
            ))
            .await
            .is_err(),
        "ordinary write preparation must still check existence"
    );
    let sequence = server.snapshot().await?.cursor.sequence;
    assert_eq!(
        server
            .rest_receipt(rest.replica_id, rest.operation.id, &identity)
            .await?,
        Some(committed.clone())
    );
    assert!(
        server
            .rest_receipt(rest.replica_id, rest.operation.id, "changed JSON")
            .await
            .is_err()
    );
    let before = server.prepare_push(&rest).await?;
    assert_eq!(
        server
            .write_rest_prepared(rest, before, vec![], identity)
            .await?,
        committed
    );
    assert_eq!(server.snapshot().await?.cursor.sequence, sequence);
    Ok(())
}

async fn drain(client: &SyncClient, server: &SyncServer) -> Result<(), OrmError> {
    drain_model::<Note>(client, server).await
}

async fn drain_model<M: SyncModel>(
    client: &SyncClient,
    server: &SyncServer,
) -> Result<(), OrmError> {
    let engine = SyncEngine::for_model::<M>(client.clone(), server.for_model::<M>()?)?;
    for _ in 0..100 {
        let report = engine.sync_once().await?;
        assert!(!report.blocked, "{report:?}");
        if !report.has_more {
            return Ok(());
        }
    }
    panic!("synchronization did not converge");
}
