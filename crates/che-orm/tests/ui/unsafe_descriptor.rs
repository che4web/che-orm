use che_orm::{Model, ModelField};

#[derive(Model)]
struct User {
    #[field(primary_key)]
    id: i64,
    email: String,
}

fn main() {
    let _ = ModelField::<User, i64>::new("email");
}
