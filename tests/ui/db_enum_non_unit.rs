use che_orm2::DbEnum;

#[derive(DbEnum)]
enum Status {
    Draft(String),
}

fn main() {}
