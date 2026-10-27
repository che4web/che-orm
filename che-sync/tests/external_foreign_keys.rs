use che_orm::{
    Database, Migration, MigrationGraph, MigrationId, MigrationOperation, Model, ProjectState,
    SchemaSet, Uuid,
};
use che_sync::{AllowAll, Registry, SyncServer};
use std::sync::Arc;

#[derive(Debug, Model)]
#[orm(table = "external_parent")]
struct Parent {
    #[orm(primary_key)]
    id: i64,
}

#[derive(Debug, Model)]
#[orm(table = "external_child", sync, sync_name = "external.child")]
struct Child {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(foreign_key = Parent, on_delete = "cascade")]
    parent_id: i64,
    #[orm(foreign_key = Parent, on_delete = "set null")]
    owner_id: Option<i64>,
    title: String,
}

#[tokio::test]
async fn ordinary_parents_capture_set_null_cascade_rollback_and_deleted_identity() {
    let schema = SchemaSet::new().model::<Parent>().server_model::<Child>();
    let registry = Registry::from_schema(&schema).unwrap();
    let state = ProjectState::from_app_schemas([("external", &schema)]).unwrap();
    let db = Database::connect_in_memory().await.unwrap();
    db.migrate(
        MigrationGraph::new(vec![Migration::new(
            MigrationId {
                app: "external".into(),
                name: "0001_initial".into(),
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
    let server = SyncServer::open_model_logs(db, registry, "test", Arc::new(AllowAll))
        .await
        .unwrap();
    let db = server.database();
    let parent = db.create::<Parent>().execute().await.unwrap();
    let owner = db.create::<Parent>().execute().await.unwrap();
    let child = db
        .create::<Child>()
        .set(Child::PARENT_ID, parent.id)
        .set(Child::OWNER_ID, Some(owner.id))
        .set(Child::TITLE, "child")
        .execute()
        .await
        .unwrap();
    let before = server.changes::<Child>(0, 100).await.unwrap();
    let rolled_back: Result<(), che_orm::OrmError> = db
        .transaction(move |tx| {
            Box::pin(async move {
                tx.delete::<Parent>(owner.id).await?;
                Err(che_orm::OrmError::Interaction("rollback".into()))
            })
        })
        .await;
    assert!(rolled_back.is_err());
    assert_eq!(
        db.get::<Child>(child.id).await.unwrap().unwrap().owner_id,
        Some(owner.id)
    );
    assert_eq!(server.changes::<Child>(0, 100).await.unwrap(), before);
    db.delete::<Parent>(owner.id).await.unwrap();
    assert_eq!(
        db.get::<Child>(child.id).await.unwrap().unwrap().owner_id,
        None
    );
    db.delete::<Parent>(parent.id).await.unwrap();
    assert!(db.get::<Child>(child.id).await.unwrap().is_none());
    let changes = server.changes::<Child>(0, 100).await.unwrap();
    assert_eq!(changes.operations.len(), 3);
    assert_eq!(
        changes.operations[2].kind,
        che_sync::model_log::ChangeKind::Delete
    );
    let replacement = db.create::<Parent>().execute().await.unwrap();
    assert!(
        db.create::<Child>()
            .set(Child::ID, child.id)
            .set(Child::PARENT_ID, replacement.id)
            .set(Child::OWNER_ID, None::<i64>)
            .set(Child::TITLE, "resurrection")
            .execute()
            .await
            .is_err()
    );
    assert_eq!(server.changes::<Child>(0, 100).await.unwrap(), changes);
}
