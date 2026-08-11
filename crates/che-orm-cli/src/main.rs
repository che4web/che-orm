use std::{
    fs,
    path::{Path, PathBuf},
};

use che_orm::{Database, Result};
#[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
use che_orm::{MigrationOptions, Schema, generate_migrations};
use clap::{Parser, Subcommand};
use serde::Deserialize;

#[derive(Debug, Parser)]
#[command(name = "che-orm")]
#[command(about = "Migration CLI for che-orm")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
    Makemigrations {
        #[arg(long, default_value = "che_orm_schema.json")]
        schema: PathBuf,

        #[arg(long, default_value = "migrations")]
        migrations_dir: PathBuf,

        #[arg(long, default_value = "auto")]
        name: String,
    },
    Migrate {
        #[arg(long)]
        database_url: Option<String>,

        #[arg(long, default_value = "app.toml")]
        config: PathBuf,

        #[arg(long, default_value = "migrations")]
        migrations_dir: PathBuf,
    },
    Status {
        #[arg(long)]
        database_url: Option<String>,

        #[arg(long, default_value = "app.toml")]
        config: PathBuf,

        #[arg(long, default_value = "migrations")]
        migrations_dir: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        #[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
        Command::Makemigrations {
            schema,
            migrations_dir,
            name,
        } => makemigrations(&schema, &migrations_dir, &name),
        Command::Migrate {
            database_url,
            config,
            migrations_dir,
        } => migrate(database_url, &config, &migrations_dir).await,
        Command::Status {
            database_url,
            config,
            migrations_dir,
        } => status(database_url, &config, &migrations_dir).await,
    }
}

#[derive(Debug, Deserialize)]
struct AppConfig {
    database: DatabaseConfig,
}

#[derive(Debug, Deserialize)]
struct DatabaseConfig {
    url: String,
}

#[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
fn makemigrations(schema_path: &Path, migrations_dir: &Path, name: &str) -> Result<()> {
    let new_schema = Schema::load(schema_path)?;
    let generated = generate_migrations(
        &new_schema,
        MigrationOptions::new(migrations_dir).named(name),
    )?;
    if generated.path.is_none() {
        println!("No schema changes detected");
        return Ok(());
    }

    println!("Created {}", generated.path.unwrap().display());
    Ok(())
}

async fn migrate(database_url: Option<String>, config: &Path, migrations_dir: &Path) -> Result<()> {
    let database_url = match database_url {
        Some(database_url) => database_url,
        None => database_url_from_config(config)?,
    };
    let db = Database::connect(&database_url)
        .await?
        .with_migrations_dir(migrations_dir);
    for name in db.migrate().await? {
        println!("Applied {name}");
    }

    Ok(())
}

async fn status(database_url: Option<String>, config: &Path, migrations_dir: &Path) -> Result<()> {
    let database_url = match database_url {
        Some(database_url) => database_url,
        None => database_url_from_config(config)?,
    };
    let db = Database::connect(&database_url)
        .await?
        .with_migrations_dir(migrations_dir);
    for migration in db.migration_status().await? {
        let state = if migration.checksum_mismatch {
            "mismatch"
        } else if migration.applied {
            "applied"
        } else {
            "pending"
        };
        println!("{state:7} {}", migration.name);
    }
    Ok(())
}

fn database_url_from_config(path: &Path) -> Result<String> {
    let config = fs::read_to_string(path)?;
    let config: AppConfig = toml::from_str(&config).map_err(std::io::Error::other)?;
    Ok(config.database.url)
}
