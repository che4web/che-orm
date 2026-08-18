use che_orm2::DbEnum;

#[derive(che_orm2::serde::Serialize, DbEnum)]
enum Status {
    Draft,
}

fn main() {}
