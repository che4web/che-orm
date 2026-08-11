use std::path::PathBuf;

use che_orm::{
    Application, DatabaseSettings, Manager, MigrationSettings, Model, ModelSchema, Result,
    RuntimeSettings, Schema,
};
use clap::{Parser, Subcommand};

#[derive(Debug, Clone, Model)]
#[model(table = "users")]
struct User {
    #[field(primary_key)]
    id: i64,
    email: String,
}

struct App;

impl Application for App {
    fn schema(&self) -> Schema {
        Schema::from_models(vec![ModelSchema::from_model::<User>()])
    }

    fn settings(&self) -> Result<RuntimeSettings> {
        Ok(RuntimeSettings {
            database: DatabaseSettings {
                url: "sqlite://example.sqlite?mode=rwc".to_string(),
            },
            migrations: MigrationSettings {
                dir: PathBuf::from("migrations"),
            },
        })
    }
}

#[derive(Debug, Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Makemigrations {
        #[arg(default_value = "auto")]
        name: String,
    },
    Migrate,
    Status,
    Ping,
}

#[tokio::main]
async fn main() -> Result<()> {
    let manager = Manager::new(App);
    match Cli::parse().command {
        Command::Makemigrations { name } => {
            let migration = manager.makemigrations(name)?;
            match migration.path {
                Some(path) => println!("Created {}", path.display()),
                None => println!("No schema changes detected"),
            }
        }
        Command::Migrate => {
            for name in manager.migrate().await? {
                println!("Applied {name}");
            }
        }
        Command::Status => {
            for migration in manager.status().await? {
                println!("{} {}", migration.applied, migration.name);
            }
        }
        Command::Ping => {
            manager.connect().await?;
            println!("Database connection is ready");
        }
    }
    Ok(())
}
