#![cfg(feature = "postgres")]

use che_orm::{
    Database, Migration, MigrationGraph, MigrationId, MigrationOperation, Model, OrmError,
    TableState, Uuid,
};
use che_sync::{AllowAll, Field, Registry, SchemaRole, SyncServer, protocol::*};
use std::sync::Arc;

#[derive(Debug, Model)]
#[orm(table = "sync_notes")]
struct Note {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(unique)]
    title: String,
    enabled: bool,
    #[orm(auto_now)]
    updated: che_orm::time::OffsetDateTime,
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
#[ignore = "requires TEST_POSTGRES_URL and a live PostgreSQL server"]
async fn postgres_commit_order_receipts_constraints_and_snapshot() {
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
    let result = scenario(db).await;
    admin
        .execute_sql(format!("DROP SCHEMA \"{schema}\" CASCADE"), vec![])
        .await
        .unwrap();
    result.unwrap();
}

async fn scenario(db: Database) -> Result<(), Box<dyn std::error::Error>> {
    let registry = Registry::new().register::<Note>()?;
    let mut ops = vec![MigrationOperation::CreateTable {
        table: TableState::from(&Note::schema()),
    }];
    ops.extend(registry.migration_operations(SchemaRole::Server));
    db.migrate(MigrationGraph::new(vec![Migration::new(
        MigrationId {
            app: "sync".into(),
            name: "0001".into(),
        },
        vec![],
        ops,
    )])?)
    .await?;
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
    let snapshot = server.snapshot().await?;
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
            cursor: cursor.clone(),
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
    Ok(())
}
