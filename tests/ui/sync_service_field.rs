use che_orm::{Model, Uuid};

#[derive(Debug, Model)]
#[orm(table = "tasks", sync)]
struct Task {
    #[orm(primary_key)]
    id: Uuid,
    revision: i64,
}

fn main() {}
