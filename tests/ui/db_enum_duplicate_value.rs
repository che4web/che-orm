use che_orm2::DbEnum;

#[derive(DbEnum)]
enum Status {
    #[db_enum(rename = "open")]
    Draft,
    #[db_enum(rename = "open")]
    Published,
}

fn main() {}
