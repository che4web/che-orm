use che_orm::{Database, Model};

#[derive(Model)]
struct User {
    #[field(primary_key)]
    id: i64,
    email: String,
}

fn main() {
    let _ = UserFields::ID.eq("not an integer");
}

fn invalid_operations(db: &Database) {
    let _ = UserFields::ID.contains("text");
    let _ = UserFields::ID.in_values(["text"]);
    let _ = db.query::<User>().sum(UserFields::EMAIL);
    let _ = db.create::<User>().set(UserFields::EMAIL, 42_i64);
    let _ = db.update::<User>(1).set(UserFields::ID, 2_i64);
}
