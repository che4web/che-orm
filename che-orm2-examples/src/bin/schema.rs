use che_orm2::{Model, SqlCompiler, SqliteDialect};
use che_orm2_examples::ExampleUser;

fn main() {
    let table = SqlCompiler::<SqliteDialect>::compile(&ExampleUser::create_table().into_ast());
    let schema = SqlCompiler::<SqliteDialect>::compile_schema(&ExampleUser::schema());

    println!("{};", table.sql);
    for index in schema.indexes {
        println!("{};", index);
    }
    for trigger in schema.triggers {
        println!("{};", trigger);
    }
}
