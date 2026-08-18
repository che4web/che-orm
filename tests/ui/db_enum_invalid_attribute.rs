use che_orm::DbEnum;

#[derive(DbEnum)]
enum Status {
    #[db_enum(value = "draft")]
    Draft,
}

fn main() {}
