use che_orm_examples::{migrations, registry};
use orm::ProjectState;

#[test]
fn compiled_history_matches_registered_models() {
    let history = migrations::graph()
        .unwrap()
        .replay(ProjectState::default())
        .unwrap();
    let registry = registry();
    let desired = ProjectState::from_app_schemas(registry.app_schemas()).unwrap();
    assert!(history.diff(&desired).unwrap().is_empty());
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn initial_migration_creates_both_tables_and_cascades_deletes() {
    use che_orm_examples::{ExamplePost, ExampleUser};
    use orm::Database;

    let db = Database::connect_in_memory().await.unwrap();
    assert_eq!(db.migrate(migrations::graph().unwrap()).await.unwrap(), 1);
    assert_eq!(db.migrate(migrations::graph().unwrap()).await.unwrap(), 0);

    let user = db
        .create::<ExampleUser>()
        .set(ExampleUser::EMAIL, "alice@example.test")
        .set(ExampleUser::NAME, "Alice")
        .execute()
        .await
        .unwrap();
    let post = db
        .create::<ExamplePost>()
        .set(ExamplePost::USER_ID, user.id)
        .set(ExamplePost::TITLE, "First post")
        .execute()
        .await
        .unwrap();
    assert!(db.delete::<ExampleUser>(user.id).await.unwrap());
    assert!(db.get::<ExamplePost>(post.id).await.unwrap().is_none());
}
