use che_orm2::DbEnum;

#[derive(DbEnum)]
enum Status {
    #[db_enum(value = "draft")]
    Draft,
}

fn main() {}
