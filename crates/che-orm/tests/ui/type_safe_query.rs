use che_orm::{Model, SqliteBackend};

#[derive(Model)]
struct User {
    #[field(primary_key)]
    id: i64,
    email: String,
}

fn main() {
    let _ = UserFields::ID.eq("not an integer");
}

fn invalid_operations(db: &SqliteBackend) {
    let _ = UserFields::ID.contains("text");
    let _ = UserFields::ID.in_values(["text"]);
    let _ = User::objects(db).query().sum(UserFields::EMAIL);
}
