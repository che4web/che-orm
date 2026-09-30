use std::{env, process::ExitCode};

use che_orm_examples::database_path;
use orm::{Migration, MigrationGraph, MigrationId, MigrationOperation, SqliteDialect};

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("manage: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    match args.as_slice() {
        [] => {
            print_help();
            Ok(())
        }
        [command] if command == "help" => {
            print_help();
            Ok(())
        }
        [command] if command == "schema" => {
            print!("{}", che_orm_examples::registry().to_sql::<SqliteDialect>());
            Ok(())
        }
        [command] if command == "migrate" => migrate(),
        [command, action] if command == "migrate" && action == "status" => status(),
        [command, ..] if command == "makemigrations" => Err(
            "the example ships a frozen compiled baseline; change its migration module before adding model changes".into(),
        ),
        _ => Err("unknown command or arguments".into()),
    }
}

fn migrations() -> Vec<Migration> {
    vec![Migration::new(
        MigrationId {
            app: "examples".into(),
            name: "0001_initial".into(),
        },
        vec![],
        vec![MigrationOperation::RunSql {
            forward: include_str!("../../migrations/20260815144721_initial_schema.sql").into(),
            reverse: None,
            state_operations: vec![],
        }],
    )]
}

fn database() -> Result<orm::rusqlite::Connection, String> {
    let path = database_path();
    if path.is_empty() || path == ":memory:" {
        return Err("compiled example migrations require a SQLite file path".into());
    }
    orm::rusqlite::Connection::open(path).map_err(|error| error.to_string())
}

fn migrate() -> Result<(), String> {
    let graph = MigrationGraph::new(migrations()).map_err(|error| error.to_string())?;
    let mut connection = database()?;
    let count = orm::migration::sqlite_executor::apply_operation_migrations_on_connection(
        &mut connection,
        graph,
    )
    .map_err(|error| error.to_string())?;
    println!("Applied {count} compiled migration(s).");
    Ok(())
}

fn status() -> Result<(), String> {
    let connection = database()?;
    let applied = orm::migration::sqlite_executor::applied_operation_migrations(&connection)
        .map_err(|error| error.to_string())?;
    for migration in migrations() {
        let marker = if applied.contains_key(&(migration.id.app.clone(), migration.id.name.clone()))
        {
            "[X]"
        } else {
            "[ ]"
        };
        println!("{marker} {}:{}", migration.id.app, migration.id.name);
    }
    Ok(())
}

fn print_help() {
    println!(
        "Usage:\n  cargo run -p che-orm-examples --bin manage -- schema\n  cargo run -p che-orm-examples --bin manage -- migrate\n  cargo run -p che-orm-examples --bin manage -- migrate status"
    );
}
