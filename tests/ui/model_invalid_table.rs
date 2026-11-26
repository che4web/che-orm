use che_orm::Model;

#[derive(Debug, Model)]
#[orm(table = "Tasks")]
struct Task {
    #[orm(primary_key)]
    id: i64,
}

#[derive(Debug, Model)]
#[orm(table = "\"tasks\"")]
struct QuotedTask {
    #[orm(primary_key)]
    id: i64,
}

fn main() {}
