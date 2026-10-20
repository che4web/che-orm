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
