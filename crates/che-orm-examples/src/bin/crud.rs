#![allow(dead_code)]

use che_orm::{Database, Model};

#[derive(Debug, Clone, Model)]
#[model(table = "users")]
struct User {
    #[field(primary_key)]
    id: i64,

    #[field(unique, max_length = 255)]
    email: String,

    name: String,

    #[field(default = true)]
    is_active: bool,
}

#[tokio::main]
async fn main() -> che_orm::Result<()> {
    let db = Database::connect("sqlite::memory:").await?;
    db.create_table::<User>().await?;

    let user = db
        .create::<User>()
        .set("email", "alice@example.com")
        .set("name", "Alice")
        .set("is_active", true)
        .execute()
        .await?;

    println!("created: {user:?}");

    let mut fetched = db.get::<User>(user.id).await?;
    println!("fetched: {fetched:?}");

    fetched.name = "Alicia".to_string();
    fetched.is_active = false;
    let updated = db.save(&fetched).await?;
    println!("updated: {updated:?}");

    let users: Vec<User> = db
        .query()
        .filter(UserFields::EMAIL.contains("@example.com"))
        .order_by(UserFields::EMAIL)
        .all()
        .await?;

    println!("all users: {users:?}");

    db.delete::<User>(user.id).await?;
    println!("deleted user id {}", user.id);

    Ok(())
}
