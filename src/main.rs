use che_orm::{Compiler, Model, models::User};

fn main() {
    let u = User::new("test".into());
    let b = User::NAME;
    println!("Hello, world! {u:?} {:?}", b);

    let insert = User::insert()
        .set(User::NAME, "Alice")
        .returning_all()
        .into_ast()
        .expect("insert query is valid");

    let compiled = Compiler::compile(&insert);
    println!("SQL: {:?}", compiled.sql);

    let create_table = Compiler::compile(&User::create_table().into_ast());
    println!("DDL: {:?}", create_table.sql);

    let schema = Compiler::compile_schema(&User::schema());
    for index in schema.indexes {
        println!("INDEX: {:?}", index);
    }
}
