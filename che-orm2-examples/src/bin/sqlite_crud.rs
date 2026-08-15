use che_orm2::Database;
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
    let alice = database
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

    Ok(())
}
