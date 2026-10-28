#![cfg(feature = "sqlite")]
use che_orm::{
    Database, Migration, MigrationGraph, MigrationId, MigrationOperation, Model, ProjectState,
    SchemaSet, Uuid,
};
use che_sync::{AllowAll, ModelLogClient, Registry, SyncServer};
use std::sync::Arc;

#[derive(Debug, Model)]
#[orm(table = "sync_folders", sync, sync_name = "test.folder")]
struct Folder {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(foreign_key=Folder, on_delete="set null")]
    parent_id: Option<Uuid>,
    name: String,
}
fn schema(client: bool) -> SchemaSet {
    if client {
        SchemaSet::new().client_model::<Folder>()
    } else {
        SchemaSet::new().server_model::<Folder>()
    }
}
async fn database(client: bool) -> Database {
    let state = ProjectState::from_app_schemas([("test", &schema(client))]).unwrap();
    let ops = state
        .models()
        .into_iter()
        .map(|model| MigrationOperation::CreateModel { model })
        .collect();
    let db = Database::connect_in_memory().await.unwrap();
    db.migrate(
        MigrationGraph::new(vec![Migration::new(
            MigrationId {
                app: "test".into(),
                name: "0001".into(),
            },
            vec![],
            ops,
        )])
        .unwrap(),
    )
    .await
    .unwrap();
    db
}
#[tokio::test]
async fn self_references_survive_backfill_and_parent_deletion() {
    let db = database(false).await;
    let parent = Uuid::from_u128(2);
    let child = Uuid::from_u128(1); // Backfill sees the child before its parent.
    db.create::<Folder>()
        .set(Folder::ID, parent)
        .set(Folder::NAME, "Parent")
        .execute()
        .await
        .unwrap();
    db.create::<Folder>()
        .set(Folder::ID, child)
        .set(Folder::PARENT_ID, Some(parent))
        .set(Folder::NAME, "Child")
        .execute()
        .await
        .unwrap();
    let server = SyncServer::open_model_logs(
        db,
        Registry::from_schema(&schema(false)).unwrap(),
        "test",
        Arc::new(AllowAll),
    )
    .await
    .unwrap();
    server.backfill_model_log::<Folder>().await.unwrap();
    let client = ModelLogClient::open(
        database(true).await,
        Registry::from_schema(&schema(true)).unwrap(),
    )
    .await
    .unwrap();
    // A dependent row can arrive in a different page from its parent.
    loop {
        let page = server
            .changes::<Folder>(client.cursor::<Folder>().await.unwrap(), 1)
            .await
            .unwrap();
        let more = page.has_more;
        client.apply(page).await.unwrap();
        if !more {
            break;
        }
    }
    assert_eq!(
        client
            .database()
            .get::<Folder>(child)
            .await
            .unwrap()
            .unwrap()
            .parent_id,
        Some(parent)
    );
    assert!(client.conflicts().await.unwrap().is_empty());
    let cursor = client.cursor::<Folder>().await.unwrap();
    server.database().delete::<Folder>(parent).await.unwrap();
    client
        .apply(server.changes::<Folder>(cursor, 100).await.unwrap())
        .await
        .unwrap();
    assert!(
        client
            .database()
            .get::<Folder>(parent)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        client
            .database()
            .get::<Folder>(child)
            .await
            .unwrap()
            .unwrap()
            .parent_id,
        None
    );
}
