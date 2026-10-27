use che_orm::Model;

#[derive(Debug, Model)]
#[orm(app = "bad-name")]
struct Post {
    #[orm(primary_key)]
    id: i64,
}

fn main() {}
