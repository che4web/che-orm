#![cfg(feature = "postgres")]
use che_orm::{
    Database, Migration, MigrationGraph, MigrationId, MigrationOperation, Model, OrmError,
    ProjectState, SchemaSet, Uuid, ValidatedWrite,
};
use che_sync::{AllowAll, ModelLogClient, Registry, SyncServer, model_log::ChangeKind};
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Model)]
#[orm(table = "documents", sync, sync_name = "documents.document")]
struct Document {
    #[orm(primary_key)]
    id: Uuid,
    title: String,
    data: che_orm::JsonField,
    number: i64,
}
#[derive(Debug, Model)]
#[orm(table = "links", sync, sync_name = "documents.link")]
struct Link {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(foreign_key = Document, on_delete = "set null")]
    document_id: Option<Uuid>,
}
fn schema(client: bool) -> SchemaSet {
    if client {
        SchemaSet::new()
            .client_model::<Document>()
            .client_model::<Link>()
    } else {
        SchemaSet::new()
            .server_model::<Document>()
            .server_model::<Link>()
    }
}
async fn database(
    url: &str,
    admin: &Database,
    namespace: &str,
    client: bool,
) -> Result<Database, OrmError> {
    // Namespace is generated below from a UUID, never supplied by a caller.
    admin
        .execute_sql(format!("CREATE SCHEMA \"{namespace}\""), vec![])
        .await?;
    let separator = if url.contains('?') { '&' } else { '?' };
    let db = Database::connect(format!(
        "{url}{separator}options=-csearch_path%3D{namespace}"
    ))
    .await?;
    let state = ProjectState::from_app_schemas([("documents", &schema(client))]).unwrap();
    db.migrate(
        MigrationGraph::new(vec![Migration::new(
            MigrationId {
                app: "documents".into(),
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
    .await?;
    Ok(db)
}
fn insert(id: Uuid, title: &str) -> ValidatedWrite<Document> {
    ValidatedWrite::Create(
        Document::insert()
            .set(Document::ID, id)
            .set(Document::TITLE, title)
            .set(
                Document::DATA,
                che_orm::JsonField(json!({"typed":[1,true,null]})),
            )
            .set(Document::NUMBER, i64::MAX),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_POSTGRES_URL and a live PostgreSQL server"]
async fn model_journals_capture_noops_set_null_and_concurrent_commits() {
    let url = std::env::var("TEST_POSTGRES_URL").expect("set TEST_POSTGRES_URL");
    let admin = Database::connect(url.clone()).await.unwrap();
    let server_schema = format!("model_log_server_{}", Uuid::now_v7().simple());
    let client_schema = format!("model_log_client_{}", Uuid::now_v7().simple());
    let db = database(&url, &admin, &server_schema, false).await.unwrap();
    let client = database(&url, &admin, &client_schema, true).await.unwrap();
    // Join captures assertion failures too so the disposable schemas are cleaned.
    let result = tokio::spawn(scenario(db, client)).await;
    for namespace in [server_schema, client_schema] {
        admin
            .execute_sql(format!("DROP SCHEMA \"{namespace}\" CASCADE"), vec![])
            .await
            .unwrap();
    }
    result.unwrap().unwrap();
}
async fn scenario(db: Database, local: Database) -> Result<(), OrmError> {
    let server = SyncServer::open_model_logs(
        db,
        Registry::from_schema(&schema(false))?,
        "user",
        Arc::new(AllowAll),
    )
    .await?;
    let client = ModelLogClient::open(local, Registry::from_schema(&schema(true))?).await?;
    let id = Uuid::now_v7();
    let link = Uuid::now_v7();
    server.write(insert(id, "original")).await?;
    server
        .write(ValidatedWrite::Create(
            Link::insert()
                .set(Link::ID, link)
                .set(Link::DOCUMENT_ID, Some(id)),
        ))
        .await?;
    client.apply(server.changes::<Link>(0, 100).await?).await?;
    assert_eq!(client.cursor::<Link>().await?, 1);
    assert!(client.database().get::<Link>(link).await?.is_none());
    client
        .apply(server.changes::<Document>(0, 100).await?)
        .await?;
    assert_eq!(
        client
            .database()
            .get::<Link>(link)
            .await?
            .unwrap()
            .document_id,
        Some(id)
    );
    let document = client.database().get::<Document>(id).await?.unwrap();
    assert_eq!(document.data.0, json!({"typed":[1,true,null]}));
    assert_eq!(document.number, i64::MAX);
    server
        .write(ValidatedWrite::Update(
            Document::update()
                .filter(Document::ID.eq(id))
                .set(Document::TITLE, "original"),
        ))
        .await?;
    assert_eq!(
        server.changes::<Document>(0, 100).await?.operations.len(),
        1
    );
    let mut writers = Vec::new();
    for n in 0..8 {
        let server = server.clone();
        writers.push(tokio::spawn(async move {
            server
                .write(insert(Uuid::now_v7(), &format!("parallel {n}")))
                .await
        }));
    }
    let mut after = 1;
    let mut received = Vec::new();
    loop {
        let finished = writers.iter().all(tokio::task::JoinHandle::is_finished);
        let page = server.changes::<Document>(after, 2).await?;
        for operation in &page.operations {
            assert!(operation.id > after);
            received.push(operation.id);
        }
        after = page.next_cursor;
        if finished && !page.has_more {
            break;
        }
        tokio::task::yield_now().await;
    }
    for writer in writers {
        writer.await.unwrap()?;
    }
    assert_eq!(received.len(), 8);
    assert_eq!(after, 9);
    server.delete::<Document>(id).await?;
    let deletion = server.changes::<Document>(after, 100).await?;
    assert_eq!(deletion.operations[0].kind, ChangeKind::Delete);
    // Other new documents are required to make the client's page contiguous.
    client
        .apply(server.changes::<Document>(1, 100).await?)
        .await?;
    assert!(!client.conflicts().await?.is_empty());
    let nulling = server.changes::<Link>(1, 100).await?;
    assert_eq!(nulling.operations[0].kind, ChangeKind::Update);
    assert_eq!(nulling.operations[0].diff[0].new_value, Some(json!(null)));
    client.apply(nulling).await?;
    assert_eq!(
        client
            .database()
            .get::<Link>(link)
            .await?
            .unwrap()
            .document_id,
        None
    );
    assert!(client.database().get::<Document>(id).await?.is_none());
    assert!(client.conflicts().await?.is_empty());
    Ok(())
}
