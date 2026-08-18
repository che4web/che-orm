use che_orm2::Model;

#[derive(Model)]
#[orm(table = "users")]
struct User {
    #[orm(primary_key)]
    id: i64,
}

#[derive(Model)]
#[orm(table = "posts")]
struct Post {
    #[orm(primary_key)]
    id: i64,
    user: i64,
    #[orm(foreign_key = User)]
    user_id: i64,
}

fn main() {}
