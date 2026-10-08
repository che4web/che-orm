use che_orm_examples::{
    ExamplePost, ExamplePostWithUserSerializer, ExampleUser, ExampleUserWithPostsSerializer,
};
use orm::Database;

#[tokio::main]
async fn main() -> Result<(), orm::OrmError> {
    let database = Database::connect(che_orm_examples::database_url()).await?;
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();

    let user = database
        .create::<ExampleUser>()
        .set(
            ExampleUser::EMAIL,
            format!("serializer-{suffix}@example.test"),
        )
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
        .filter(ExampleUser::ID.eq(user.id))
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
        .filter(ExamplePost::USER_ID.eq(user.id))
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
