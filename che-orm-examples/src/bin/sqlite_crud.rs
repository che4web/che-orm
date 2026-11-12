use che_orm_examples::{ExamplePost, ExampleUser};
use orm::Database;

#[tokio::main]
async fn main() -> Result<(), orm::OrmError> {
    let database = Database::connect(che_orm_examples::database_url()).await?;

    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_nanos();
    let alice_email = format!("alice-{suffix}@example.test");
    let bob_email = format!("bob-{suffix}@example.test");
    let mut alice = database
        .create::<ExampleUser>()
        .set(ExampleUser::EMAIL, alice_email.as_str())
        .set(ExampleUser::NAME, "Alice")
        .execute()
        .await?;
    let bob = database
        .create::<ExampleUser>()
        .set(ExampleUser::EMAIL, bob_email.as_str())
        .set(ExampleUser::NAME, "Bob")
        .execute()
        .await?;

    let post = database
        .create::<ExamplePost>()
        .set(ExamplePost::USER_ID, alice.id)
        .set(ExamplePost::TITLE, "Alice's first post")
        .execute()
        .await?;

    alice.name = "Alice Cooper".into();
    // save updates an existing row; use the returned model for managed timestamps.
    let created_at = alice.created_at;
    alice = database
        .save(&alice)
        .await?
        .ok_or_else(|| orm::OrmError::Interaction("Alice no longer exists".into()))?;
    assert_eq!(alice.name, "Alice Cooper");
    assert_eq!(alice.created_at, created_at);
    println!("saved existing user: {alice:?}");

    let users = database
        .query::<ExampleUser>()
        .filter(ExampleUser::EMAIL.eq(alice_email))
        .all()
        .await?;

    for user in users {
        println!("{user:?}");
    }

    let posts = database.fetch_by(ExamplePost::USER_ID, alice.id).await?;
    println!("created post: {post:?}");
    println!("posts for Alice: {posts:?}");

    let _ = database.get::<ExampleUser>(bob.id).await?;

    // save never recreates a missing row.
    database.delete::<ExampleUser>(bob.id).await?;
    assert!(database.save(&bob).await?.is_none());

    Ok(())
}
