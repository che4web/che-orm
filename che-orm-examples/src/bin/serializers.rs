use che_orm_examples::{
    ExamplePost, ExamplePostWithUserSerializer, ExampleUser, ExampleUserWithPostsSerializer,
};
use orm::Database;

#[tokio::main]
async fn main() -> Result<(), orm::OrmError> {
    let database = Database::connect_in_memory()?;
    database.create_table::<ExampleUser>().await?;
    database.create_table::<ExamplePost>().await?;

    let user = database
        .create::<ExampleUser>()
        .set(ExampleUser::EMAIL, "alice@example.test")
        .set(ExampleUser::NAME, "Alice")
        .set(ExampleUser::IS_ACTIVE, true)
        .execute()
        .await?;
    database
        .create::<ExamplePost>()
        .set(ExamplePost::USER_ID, user.id)
        .set(ExamplePost::TITLE, "First post")
        .execute()
        .await?;

    // prefetch_related performs the users query and one batched posts query.
    let users = database
        .query::<ExampleUser>()
        .prefetch_related(ExamplePost::USER.reverse())
        .all(&database)
        .await?;
    let users_json = ExampleUserWithPostsSerializer::many(users);
    println!(
        "users: {}",
        serde_json::to_string_pretty(&users_json).unwrap()
    );

    // select_related materializes the related user before serialization.
    let posts = database
        .query::<ExamplePost>()
        .select_related(ExamplePost::USER)
        .all(&database)
        .await?;
    let posts_json = ExamplePostWithUserSerializer::many(posts);
    println!(
        "posts: {}",
        serde_json::to_string_pretty(&posts_json).unwrap()
    );

    Ok(())
}
