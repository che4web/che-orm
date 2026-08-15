use che_orm2::{Database, Model};
use che_orm2_examples::ExampleUser;

#[tokio::main]
async fn main() -> Result<(), che_orm2::OrmError> {
    let database = Database::connect_in_memory()?;
    database.create_table::<ExampleUser>().await?;

    database
        .transaction(|connection| -> che_orm2::rusqlite::Result<()> {
            connection.execute(
                "INSERT INTO examples_users (email, name, is_active) VALUES (?1, ?2, ?3)",
                ("committed@example.test", "Committed", true),
            )?;
            Ok(())
        })
        .await?;

    let rollback = database
        .transaction(|connection| -> che_orm2::rusqlite::Result<()> {
            connection.execute(
                "INSERT INTO examples_users (email, name, is_active) VALUES (?1, ?2, ?3)",
                ("rolled-back@example.test", "Rolled back", true),
            )?;
            Err(che_orm2::rusqlite::Error::InvalidQuery)
        })
        .await;

    assert!(rollback.is_err());
    let users = database.fetch_all(ExampleUser::query()).await?;
    println!("users after rollback: {}", users.len());

    Ok(())
}
