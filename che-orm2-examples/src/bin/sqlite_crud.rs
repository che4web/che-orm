use che_orm2::{Database, Model};
use che_orm2_examples::ExampleUser;

#[tokio::main]
async fn main() -> Result<(), che_orm2::OrmError> {
    let database = Database::connect_in_memory()?;
    database.create_table::<ExampleUser>().await?;

    let alice = ExampleUser::new("alice@example.test", "Alice");
    let bob = ExampleUser::new("bob@example.test", "Bob");
    database.insert(&alice).await?;
    database.insert(&bob).await?;

    let users = database
        .fetch_all(ExampleUser::query().filter(ExampleUser::NAME.eq("Alice")))
        .await?;

    for user in users {
        println!("{user:?}");
    }

    Ok(())
}
