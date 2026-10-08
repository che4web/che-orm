use che_orm::{Model, Uuid};

#[derive(Debug, Model)]
#[orm(table = "users")]
struct User {
    #[orm(primary_key)]
    id: Uuid,
}

#[derive(Debug, Model)]
#[orm(table = "posts")]
struct Post {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = User)]
    user_id: i64,
}

fn main() {}
