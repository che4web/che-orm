use che_orm::ProjectState;

use template_single::{migrations, registry};

#[test]
fn compiled_history_matches_registered_models() {
    let historical = migrations::graph()
        .unwrap()
        .replay(ProjectState::default())
        .unwrap();
    let registry = registry();
    let desired = ProjectState::from_app_schemas(registry.app_schemas()).unwrap();
    assert!(
        historical.diff(&desired).unwrap().is_empty(),
        "Model changes require a migration: cargo run -p template-single --bin template-single-manage -- makemigrations"
    );
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn migrations_and_crud_work_without_an_external_database() {
    use che_orm::Database;

    use template_single::models::Task;

    let database = Database::connect_in_memory().await.unwrap();
    assert_eq!(
        database
            .migrate(migrations::graph().unwrap())
            .await
            .unwrap(),
        migrations::all().len()
    );
    assert_eq!(
        database
            .migrate(migrations::graph().unwrap())
            .await
            .unwrap(),
        0
    );
    let task = database
        .create::<Task>()
        .set(Task::TITLE, "Test compiled migrations")
        .execute()
        .await
        .unwrap();
    assert!(!task.done);
    let id = task.id;
    let created_at = task.created_at;
    let updated = database
        .transaction(move |tx| {
            Box::pin(async move {
                let mut task = task;
                task.done = true;
                tx.save(&task).await
            })
        })
        .await
        .unwrap()
        .unwrap();
    assert!(updated.done);
    assert_eq!(updated.created_at, created_at);
    let stored = database.get::<Task>(id).await.unwrap().unwrap();
    assert!(stored.done);
    assert_eq!(stored.updated_at, updated.updated_at);
    let completed = database
        .query::<Task>()
        .filter(Task::DONE.eq(true))
        .order_by(Task::CREATED_AT.desc())
        .all()
        .await
        .unwrap();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].id, id);

    let rollback: Result<(), che_orm::OrmError> = database
        .transaction(move |tx| {
            Box::pin(async move {
                let mut task = tx.get::<Task>(id).await?.unwrap();
                task.done = false;
                assert!(!tx.save(&task).await?.unwrap().done);
                Err(che_orm::OrmError::Interaction("requested rollback".into()))
            })
        })
        .await;
    assert!(rollback.is_err());
    let stored = database.get::<Task>(id).await.unwrap().unwrap();
    assert!(stored.done);
    assert_eq!(stored.updated_at, updated.updated_at);

    assert!(database.delete::<Task>(id).await.unwrap());
    assert!(database.get::<Task>(id).await.unwrap().is_none());
    assert!(database.save(&updated).await.unwrap().is_none());
}
