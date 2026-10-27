use che_orm::Model;

const APP_NAME: &str = "bad-name";

#[derive(Debug, Model)]
#[orm(app = APP_NAME)]
struct Post {
    #[orm(primary_key)]
    id: i64,
}

fn main() {}
