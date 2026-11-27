use std::{env, process::ExitCode};

use che_orm_examples::{database_url, migrations};
use orm::{CurrentDialect, Database};

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

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

async fn run(args: Vec<String>) -> Result<()> {
    match args.as_slice() {
        [] => {
            print_help();
        }
        [command] if command == "help" => {
            print_help();
        }
        [command] if command == "schema" => {
            print!("{}", che_orm_examples::registry().to_sql::<CurrentDialect>());
        }
        [command, app, name] if command == "sqlmigrate" => {
            let id = orm::MigrationId {
                app: app.clone(),
                name: name.clone(),
            };
            print!("{}", migrations::graph()?.migration_sql_script(&id)?);
        }
        [command, action] if command == "migrate" && action == "status" => {
            let history = database().await?.migration_history().await?;
            let graph = migrations::graph()?;
            orm::pending_operation_migrations(&graph, &history).map_err(|e| e.to_string())?;
            for migration in graph.ordered() {
                let key = (migration.id.app.clone(), migration.id.name.clone());
                let marker = if history.contains_key(&key) { "X" } else { " " };
                println!("[{marker}] {}:{}", migration.id.app, migration.id.name);
            }
        }
        [command, args @ ..] if command == "migrate" => {
            let plan = args.iter().any(|arg| arg == "--plan");
            let positional: Vec<_> = args.iter().filter(|arg| arg.as_str() != "--plan").collect();
            let target = match positional.as_slice() {
                [] => None,
                [app, name] => Some(orm::MigrationId {
                    app: app.to_string(),
                    name: name.to_string(),
                }),
                _ => return Err("use migrate [APP NAME] [--plan]".into()),
            };
            let graph = migrations::graph()?;
            let database = database().await?;
            if plan {
                let history = database.migration_history().await?;
                let steps = graph.pending_to(&history, target.as_ref()).map_err(|e| e.to_string())?;
                if steps.is_empty() {
                    println!("No pending migrations.");
                }
                for step in steps {
                    println!("{}:{}", step.id.app, step.id.name);
                    for op in &step.operations {
                        println!("  {}", op.summary());
                    }
                }
            } else {
                let count = database.migrate_to(graph, target).await?;
                println!("Applied {count} compiled migration(s).");
            }
        }
        [command, ..] if command == "makemigrations" => return Err(
            "the example ships a frozen compiled baseline; change its migration module before adding model changes".into(),
        ),
        _ => return Err("unknown command or arguments".into()),
    }
    Ok(())
}

async fn database() -> Result<Database> {
    let url = database_url();
    if url.is_empty() {
        return Err("database URL must not be empty".into());
    }
    #[cfg(feature = "sqlite")]
    if url == ":memory:" || url == "sqlite://:memory:" {
        return Err("compiled example migrations require a SQLite file path".into());
    }
    Ok(Database::connect(url).await?)
}

fn print_help() {
    println!(
        "Usage:\n  cargo run -p che-orm-examples --bin manage -- schema\n  cargo run -p che-orm-examples --bin manage -- migrate [APP NAME] [--plan]\n  cargo run -p che-orm-examples --bin manage -- migrate status\n  cargo run -p che-orm-examples --bin manage -- sqlmigrate APP NAME"
    );
}
