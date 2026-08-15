use che_orm2::Database;
use che_orm2_examples::ExampleUser;

#[tokio::main]
async fn main() -> Result<(), che_orm2::OrmError> {
    let database = Database::connect_configured()?;

    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_nanos();
    let committed_email = format!("committed-{suffix}@example.test");
    let rolled_back_email = format!("rolled-back-{suffix}@example.test");

    database
        .transaction(move |connection| -> che_orm2::rusqlite::Result<()> {
            connection.execute(
                "INSERT INTO users (email, name, is_active) VALUES (?1, ?2, ?3)",
                (&committed_email, "Committed", true),
            )?;
            Ok(())
        })
        .await?;

    let rollback = database
        .transaction(move |connection| -> che_orm2::rusqlite::Result<()> {
            connection.execute(
                "INSERT INTO users (email, name, is_active) VALUES (?1, ?2, ?3)",
                (&rolled_back_email, "Rolled back", true),
            )?;
            Err(che_orm2::rusqlite::Error::InvalidQuery)
        })
        .await;

    assert!(rollback.is_err());
    let users = database.all::<ExampleUser>().await?;
    println!("users after rollback: {}", users.len());

    Ok(())
}
