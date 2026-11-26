use che_orm_examples::ExampleUser;
use orm::{Compiler, Model};

fn main() {
    let user = ExampleUser::new("demo@example.test", "Demo");
    println!("Example user: {user:?}");

    let insert = ExampleUser::insert()
        .set(ExampleUser::EMAIL, "alice@example.test")
        .set(ExampleUser::NAME, "Alice")
        .returning_all()
        .into_ast()
        .expect("insert query is valid");
    println!("SQL: {}", Compiler::compile(&insert).sql);

    let create_table = Compiler::compile(&ExampleUser::create_table().into_ast());
    println!("DDL: {}", create_table.sql);
}
