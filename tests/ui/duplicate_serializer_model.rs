use che_orm::{Model, ModelSerializer};

#[derive(Model)]
#[orm(table = "users")]
struct User {
    #[orm(primary_key)]
    id: i64,
}

#[derive(ModelSerializer)]
#[serializer(model = User)]
#[serializer(model = User)]
struct UserSerializer {
    id: i64,
}

fn main() {}
