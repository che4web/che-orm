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

    database
        .transaction(move |tx| {
            Box::pin(async move {
                tx.create::<ExampleUser>()
                    .set(ExampleUser::EMAIL, committed_email)
                    .set(ExampleUser::NAME, "Committed")
                    .set(ExampleUser::IS_ACTIVE, true)
                    .execute()
                    .await?;
                Ok(())
            })
        })
        .await?;

    let rollback: Result<(), orm::OrmError> = database
        .transaction(move |tx| {
            Box::pin(async move {
                tx.create::<ExampleUser>()
                    .set(ExampleUser::EMAIL, rolled_back_email)
                    .set(ExampleUser::NAME, "Rolled back")
                    .set(ExampleUser::IS_ACTIVE, true)
                    .execute()
                    .await?;
                Err(orm::OrmError::Interaction("requested rollback".into()))
            })
        })
        .await;

    assert!(rollback.is_err());
    let users = database.all::<ExampleUser>().await?;
    println!("users after rollback: {}", users.len());

    Ok(())
}
