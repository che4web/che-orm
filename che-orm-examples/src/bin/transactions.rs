use che_orm_examples::ExampleUser;
use orm::Database;

#[tokio::main]
async fn main() -> Result<(), orm::OrmError> {
    let database = Database::connect(che_orm_examples::database_url()).await?;

    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_nanos();
    let committed_email = format!("committed-{suffix}@example.test");
    let rolled_back_email = format!("rolled-back-{suffix}@example.test");

    let committed = database
        .transaction(move |tx| {
            Box::pin(async move {
                let mut user = tx
                    .create::<ExampleUser>()
                    .set(ExampleUser::EMAIL, committed_email)
                    .set(ExampleUser::NAME, "Committed")
                    .set(ExampleUser::IS_ACTIVE, true)
                    .execute()
                    .await?;
                user.name = "Saved in transaction".into();
                tx.save(&user).await?.ok_or_else(|| {
                    orm::OrmError::Interaction("created user no longer exists".into())
                })
            })
        })
        .await?;
    let stored = database.get::<ExampleUser>(committed.id).await?.unwrap();
    assert_eq!(stored.name, "Saved in transaction");

    let rollback_email = rolled_back_email.clone();
    let rollback: Result<(), orm::OrmError> = database
        .transaction(move |tx| {
            Box::pin(async move {
                let mut user = tx
                    .create::<ExampleUser>()
                    .set(ExampleUser::EMAIL, rollback_email)
                    .set(ExampleUser::NAME, "Rolled back")
                    .set(ExampleUser::IS_ACTIVE, true)
                    .execute()
                    .await?;
                user.is_active = false;
                let saved = tx.save(&user).await?;
                assert!(!saved.unwrap().is_active);
                Err(orm::OrmError::Interaction("requested rollback".into()))
            })
        })
        .await;

    assert!(rollback.is_err());
    assert!(
        database
            .query::<ExampleUser>()
            .filter(ExampleUser::EMAIL.eq(rolled_back_email))
            .first(&database)
            .await?
            .is_none()
    );
    let users = database.all::<ExampleUser>().await?;
    println!("users after rollback: {}", users.len());

    Ok(())
}
