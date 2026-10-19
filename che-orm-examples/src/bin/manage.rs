use std::{env, process::ExitCode};

use che_orm_examples::database_url;
use orm::{CurrentDialect, Database, Migration, MigrationGraph, MigrationId, MigrationOperation};

#[tokio::main]
async fn main() -> ExitCode {
    match run(env::args().skip(1).collect()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("manage: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Vec<String>) -> Result<(), String> {
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
            print!("{}", che_orm_examples::registry().to_sql::<CurrentDialect>());
            Ok(())
        }
        [command] if command == "migrate" => migrate().await,
        [command, action] if command == "migrate" && action == "status" => status().await,
        [command, ..] if command == "makemigrations" => Err(
            "the example ships a frozen compiled baseline; change its migration module before adding model changes".into(),
        ),
        _ => Err("unknown command or arguments".into()),
    }
}

fn migrations() -> Vec<Migration> {
    #[cfg(feature = "sqlite")]
    let sql = include_str!("../../migrations/20260815144721_initial_schema.sql");
    #[cfg(feature = "postgres")]
    let sql = include_str!("../../migrations/postgres_initial.sql");
    vec![Migration::new(
        MigrationId {
            app: "examples".into(),
            name: "0001_initial".into(),
        },
        vec![],
        vec![MigrationOperation::RunSql {
            forward: sql.into(),
            reverse: None,
            state_operations: vec![],
        }],
    )]
}

async fn database() -> Result<Database, String> {
    let url = database_url();
    if url.is_empty() {
        return Err("database URL must not be empty".into());
    }
    #[cfg(feature = "sqlite")]
    if url == ":memory:" || url == "sqlite://:memory:" {
        return Err("compiled example migrations require a SQLite file path".into());
    }
    Database::connect(url)
        .await
        .map_err(|error| error.to_string())
}

async fn migrate() -> Result<(), String> {
    let graph = MigrationGraph::new(migrations()).map_err(|error| error.to_string())?;
    let database = database().await?;
    let count = database
        .migrate(graph)
        .await
        .map_err(|error| error.to_string())?;
    println!("Applied {count} compiled migration(s).");
    Ok(())
}

async fn status() -> Result<(), String> {
    let database = database().await?;
    let applied = database
        .migration_history()
        .await
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
