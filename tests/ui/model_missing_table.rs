use che_orm::Model;

#[derive(Debug, Model)]
struct Post {
    #[orm(primary_key)]
    id: i64,
}

fn main() {}
