#![allow(dead_code)]

use std::path::PathBuf;

use che_orm::{Model, ModelSchema, Schema, create_table_sql};

#[derive(Debug, Clone, Model)]
#[model(table = "users")]
struct User {
    #[field(primary_key)]
    id: i64,
    email: String,
    name: String,
}

#[derive(Debug, Clone, Model)]
#[model(table = "posts")]
struct Post {
    #[field(primary_key)]
    id: i64,

    #[field(foreign_key = User)]
    user_id: i64,

    title: String,
}

fn main() -> che_orm::Result<()> {
    let schema = Schema::from_models(vec![
        ModelSchema::from_model::<User>(),
        ModelSchema::from_model::<Post>(),
    ]);
    let path = PathBuf::from("che_orm_schema.json");
    schema.save(&path)?;

    println!("saved schema snapshot to {}", path.display());
    println!("\nusers table SQL:\n{}", create_table_sql::<User>());
    println!("\nposts table SQL:\n{}", create_table_sql::<Post>());

    Ok(())
}
