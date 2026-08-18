use che_orm::DbEnum;

#[derive(che_orm::serde::Serialize, DbEnum)]
enum Status {
    Draft,
}

fn main() {}
