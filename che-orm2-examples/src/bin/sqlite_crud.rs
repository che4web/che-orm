use che_orm2::{Database, Model};
use che_orm2_examples::{ExamplePost, ExampleUser};

#[tokio::main]
async fn main() -> Result<(), che_orm2::OrmError> {
    let database = Database::connect_configured()?;

    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_nanos();
    let alice_email = format!("alice-{suffix}@example.test");
    let bob_email = format!("bob-{suffix}@example.test");
    let alice = ExampleUser::new(&alice_email, "Alice");
    let bob = ExampleUser::new(&bob_email, "Bob");
    let alice_id = database.insert(&alice).await?.last_insert_rowid.unwrap();
    database.insert(&bob).await?;

    database
        .insert(&ExamplePost {
            id: 0,
            user_id: alice_id,
            title: "Alice's first post".into(),
        })
        .await?;

    let users = database
        .fetch_all(ExampleUser::query().filter(ExampleUser::EMAIL.eq(alice_email)))
        .await?;

    for user in users {
        println!("{user:?}");
    }

    let posts = database.fetch_by(ExamplePost::USER_ID, alice_id).await?;
    println!("posts for Alice: {posts:?}");

    Ok(())
}
