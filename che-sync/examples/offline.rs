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
    #[orm(default = "0")]
    revision: i64,
    #[orm(auto_now)]
    updated_at: che_orm::time::OffsetDateTime,
    deleted_at: Option<che_orm::time::OffsetDateTime>,
    #[orm(default = "''")]
    sync_scope: String,
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

async fn database(
    registry: &Registry,
    role: SchemaRole,
) -> Result<(Database, Option<(Database, String)>), Box<dyn std::error::Error>> {
    #[cfg(feature = "sqlite")]
    let db = Database::connect_in_memory().await?;
    #[cfg(feature = "sqlite")]
    let cleanup = None;
    #[cfg(feature = "postgres")]
    let (db, cleanup) = {
        let url = std::env::var("CHE_SYNC_DATABASE_URL")?;
        let admin = Database::connect(url.clone()).await?;
        let schema = format!("offline_example_{}", Uuid::now_v7().simple());
        admin
            .execute_sql(format!("CREATE SCHEMA \"{schema}\""), vec![])
            .await?;
        let separator = if url.contains('?') { '&' } else { '?' };
        let db =
            Database::connect(format!("{url}{separator}options=-csearch_path%3D{schema}")).await?;
        (db, Some((admin, schema)))
    };
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
    Ok((db, cleanup))
}

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registry = Registry::new().register::<Note>()?;
    let (server_db, server_cleanup) = database(&registry, SchemaRole::Server).await?;
    let (first_db, first_cleanup) = database(&registry, SchemaRole::Client).await?;
    let (second_db, second_cleanup) = database(&registry, SchemaRole::Client).await?;
    let server = SyncServer::open(
        server_db,
        registry.clone(),
        "demo",
        "demo-user",
        Arc::new(AllowAll),
    )
    .await?;
    let a = SyncClient::open(first_db, registry.clone(), "demo").await?;
    let b = SyncClient::open(second_db, registry, "demo").await?;

    let note = Note {
        id: Uuid::now_v7(),
        title: "Offline note".into(),
        body: "Initial body".into(),
        revision: 0,
        updated_at: che_orm::time::OffsetDateTime::UNIX_EPOCH,
        deleted_at: None,
        sync_scope: String::new(),
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
    for (admin, schema) in [server_cleanup, first_cleanup, second_cleanup]
        .into_iter()
        .flatten()
    {
        admin
            .execute_sql(format!("DROP SCHEMA \"{schema}\" CASCADE"), vec![])
            .await?;
    }
    Ok(())
}
