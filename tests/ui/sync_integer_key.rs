use che_orm::Model;

#[derive(Debug, Model)]
#[orm(table = "tasks", sync)]
struct Task {
    #[orm(primary_key)]
    id: i64,
    title: String,
}

fn main() {}
