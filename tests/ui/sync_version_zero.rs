use che_orm::{Model, Uuid};

#[derive(Debug, Model)]
#[orm(table = "tasks", sync, sync_version = 0)]
struct Task {
    #[orm(primary_key)]
    id: Uuid,
}

fn main() {}
