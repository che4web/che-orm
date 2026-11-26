use che_orm::Model;

const APP_NAME: &str = "Bad_Name";

#[derive(Debug, Model)]
#[orm(app = APP_NAME)]
struct Post {
    #[orm(primary_key)]
    id: i64,
}

fn main() {}
