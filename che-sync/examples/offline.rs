//! Two offline clients and an in-process server. Every database is migrated
//! before use; production applications freeze these operations in their history.
use che_orm::{
    Database, Migration, MigrationGraph, MigrationId, MigrationOperation, Model, ModelRole,
    OrmError, ProjectState, SchemaSet, Uuid,
};
use che_sync::{AllowAll, SyncClient, SyncEngine, SyncServer};
use std::sync::Arc;

#[derive(Debug, Model)]
#[orm(table = "notes", sync, sync_name = "notes.note")]
struct Note {
    #[orm(primary_key)]
    id: Uuid,
    title: String,
    body: String,
}

async fn database(
    schema: &SchemaSet,
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
    let operations = ProjectState::from_app_schemas([("notes", schema)])?
        .models()
        .into_iter()
        .map(|model| MigrationOperation::CreateModel { model })
        .collect();
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

async fn flush(engine: &SyncEngine<SyncServer>) -> Result<(), OrmError> {
    loop {
        let report = engine.flush().await?;
        println!("Outgoing changes: {report:?}");
        if !report.has_more {
            break;
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server_schema = SchemaSet::new().server_model::<Note>();
    let client_schema = SchemaSet::new().client_model::<Note>();
    let (server_db, server_cleanup) = database(&server_schema).await?;
    let (first_db, first_cleanup) = database(&client_schema).await?;
    let (second_db, second_cleanup) = database(&client_schema).await?;
    let server = SyncServer::from_schema(
        server_db,
        &server_schema,
        "demo",
        "demo-user",
        Arc::new(AllowAll),
    )
    .await?;
    let a = SyncClient::from_schema(first_db, &client_schema, "demo").await?;
    let b = SyncClient::from_schema(second_db, &client_schema, "demo").await?;
    assert_eq!(
        server.database().model_role::<Note>(),
        Some(ModelRole::Server)
    );
    assert_eq!(a.database().model_role::<Note>(), Some(ModelRole::Client));
    let a_feed = a.model::<Note, _>(server.for_model::<Note>()?)?;
    let b_feed = b.model::<Note, _>(server.for_model::<Note>()?)?;

    let note = Note {
        id: Uuid::now_v7(),
        title: "Offline note".into(),
        body: "Initial body".into(),
    };
    a.database().insert(&note).await?;
    println!(
        "Created offline: {:?}",
        a.database().get::<Note>(note.id).await?
    );
    // Initial loading is explicit and preserves the offline create.
    println!("Initial pull: {:?}", a_feed.pull().await?);
    a_feed.full_load().await?;
    flush(&a_feed).await?;
    b_feed.full_load().await?;

    a.database()
        .update::<Note>(note.id)
        .set(Note::TITLE, "Title from device A")
        .execute()
        .await?;
    b.database()
        .update::<Note>(note.id)
        .set(Note::BODY, "Body from device B")
        .execute()
        .await?;
    flush(&a_feed).await?;
    // Receive without submitting device B's offline edit.
    println!("Download only: {:?}", b_feed.pull().await?);
    flush(&b_feed).await?;
    a_feed.pull().await?;
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
