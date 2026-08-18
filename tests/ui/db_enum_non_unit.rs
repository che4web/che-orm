use che_orm::DbEnum;

#[derive(DbEnum)]
enum Status {
    Draft(String),
}

fn main() {}
