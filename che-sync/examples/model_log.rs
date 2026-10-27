//! Protocol-2 journal replay and offline field merging without an HTTP adapter.
//! These disposable databases derive migrations for demonstration only;
//! persistent applications must freeze their compiled migration operations.
use che_orm::{
    Database, Migration, MigrationGraph, MigrationId, MigrationOperation, Model, ProjectState,
    SchemaSet, Uuid,
};
use che_sync::{AllowAll, ModelLogClient, Registry, SyncServer};
use std::sync::Arc;

#[derive(Debug, Model)]
#[orm(table = "notes", sync, sync_name = "notes.note")]
struct Note {
    #[orm(primary_key)]
    id: Uuid,
    title: String,
    body: String,
}

async fn database(schema: &SchemaSet) -> Result<Database, Box<dyn std::error::Error>> {
    let operations = ProjectState::from_app_schemas([("notes", schema)])?
        .models()
        .into_iter()
        .map(|model| MigrationOperation::CreateModel { model })
        .collect();
    let db = Database::connect_in_memory().await?;
    db.migrate(MigrationGraph::new(vec![Migration::new(
        MigrationId {
            app: "model_log_example".into(),
            name: "0001_initial".into(),
        },
        vec![],
        operations,
    )])?)
    .await?;
    Ok(db)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server_schema = SchemaSet::new().server_model::<Note>();
    let client_schema = SchemaSet::new().client_model::<Note>();
    let server = SyncServer::open_model_logs(
        database(&server_schema).await?,
        Registry::from_schema(&server_schema)?,
        "demo-user",
        Arc::new(AllowAll),
    )
    .await?;
    let client = ModelLogClient::open(
        database(&client_schema).await?,
        Registry::from_schema(&client_schema)?,
    )
    .await?;

    let note = Note {
        id: Uuid::now_v7(),
        title: "Original title".into(),
        body: "Original body".into(),
    };
    server.database().insert(&note).await?;

    // Protocol 2 starts at cursor zero and needs no snapshot/full_load step.
    let page = server.changes::<Note>(0, 100).await?;
    assert!(!page.has_more);
    client.apply(page).await?;

    // The configured client handle captures an offline edit transactionally.
    client
        .database()
        .update::<Note>(note.id)
        .set(Note::TITLE, "Offline title")
        .execute()
        .await?;
    server
        .database()
        .update::<Note>(note.id)
        .set(Note::BODY, "Server body")
        .execute()
        .await?;

    // Applying a later page merges independent fields and retains local work.
    let cursor = client.cursor::<Note>().await?;
    let page = server.changes::<Note>(cursor, 100).await?;
    assert!(!page.has_more);
    client.apply(page).await?;
    let merged = client.database().get::<Note>(note.id).await?.unwrap();
    assert_eq!(merged.title, "Offline title");
    assert_eq!(merged.body, "Server body");
    assert!(client.conflicts().await?.is_empty());
    println!("Merged locally: {merged:?}");
    println!("The offline title remains pending; use ModelLogTransport to deliver it.");
    Ok(())
}
