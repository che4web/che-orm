use che_orm::Model;

#[derive(Debug, Model)]
#[orm(app = String::new())]
struct Post {
    #[orm(primary_key)]
    id: i64,
}

fn main() {}
