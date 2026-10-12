//! Two offline clients and an in-process server. Every database is migrated
//! before use; production applications freeze these operations in their history.
use che_orm::{
    Database, Migration, MigrationGraph, MigrationId, MigrationOperation, Model, OrmError,
    TableState, Uuid,
};
use che_sync::{
    AllowAll, Field, Registry, SchemaRole, SyncClient, SyncEngine, SyncModel, SyncServer,
};
use std::sync::Arc;

#[derive(Debug, Model)]
#[orm(table = "notes")]
struct Note {
    #[orm(primary_key)]
    id: Uuid,
    title: String,
    body: String,
}

impl SyncModel for Note {
    const SYNC_NAME: &'static str = "notes.note";
    fn sync_fields() -> Vec<Field> {
        vec![
            Field::server_owned("id", "id"),
            Field::writable("title", "title"),
            Field::writable("body", "body"),
        ]
    }
}

#[cfg(feature = "sqlite")]
async fn database(
    registry: &Registry,
    role: SchemaRole,
) -> Result<Database, Box<dyn std::error::Error>> {
    let db = Database::connect_in_memory().await?;
    let mut operations = vec![MigrationOperation::CreateTable {
        table: TableState::from(&Note::schema()),
    }];
    operations.extend(registry.migration_operations(role));
    let graph = MigrationGraph::new(vec![Migration::new(
        MigrationId {
            app: "offline_example".into(),
            name: "0001_initial".into(),
        },
        vec![],
        operations,
    )])?;
    db.migrate(graph).await?;
    Ok(db)
}

#[cfg(feature = "sqlite")]
async fn synchronize(client: &SyncClient, server: &SyncServer) -> Result<(), OrmError> {
    let engine = SyncEngine::new(client.clone(), server.clone());
    loop {
        let report = engine.sync_once().await?;
        println!("Sync: {report:?}");
        if !report.has_more {
            break;
        }
    }
    Ok(())
}

#[cfg(feature = "sqlite")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registry = Registry::new().register::<Note>()?;
    let server = SyncServer::open(
        database(&registry, SchemaRole::Server).await?,
        registry.clone(),
        "demo",
        "demo-user",
        Arc::new(AllowAll),
    )
    .await?;
    let a = SyncClient::open(
        database(&registry, SchemaRole::Client).await?,
        registry.clone(),
        "demo",
    )
    .await?;
    let b = SyncClient::open(
        database(&registry, SchemaRole::Client).await?,
        registry,
        "demo",
    )
    .await?;

    let note = Note {
        id: Uuid::now_v7(),
        title: "Offline note".into(),
        body: "Initial body".into(),
    };
    a.save(&note).await?;
    println!(
        "Created offline: {:?}",
        a.database().get::<Note>(note.id).await?
    );
    synchronize(&a, &server).await?;
    synchronize(&b, &server).await?;

    let mut a_note = a.database().get::<Note>(note.id).await?.unwrap();
    let mut b_note = b.database().get::<Note>(note.id).await?.unwrap();
    a_note.title = "Title from device A".into();
    b_note.body = "Body from device B".into();
    a.save(&a_note).await?;
    b.save(&b_note).await?;
    synchronize(&a, &server).await?;
    synchronize(&b, &server).await?;
    synchronize(&a, &server).await?;
    println!("Merged: {:?}", a.database().get::<Note>(note.id).await?);
    Ok(())
}

#[cfg(not(feature = "sqlite"))]
fn main() {
    println!("Run this in-memory example with the sqlite feature.");
}
