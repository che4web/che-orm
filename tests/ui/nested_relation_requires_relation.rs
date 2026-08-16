use che_orm2::{Model, ModelSerializer};

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
    #[orm(foreign_key = User)]
    user_id: i64,
}

#[derive(ModelSerializer)]
#[serializer(model = User)]
struct UserSerializer {
    id: i64,
    #[serializer(many = Post)]
    posts: Vec<PostSerializer>,
}

struct PostSerializer;

fn main() {}
