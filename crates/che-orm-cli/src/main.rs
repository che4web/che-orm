use std::{
    fs,
    path::{Path, PathBuf},
};

use che_orm::{Result, Schema, SqliteBackend, diff_schemas, sqlite_migration_sql};
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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
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

fn makemigrations(schema_path: &Path, migrations_dir: &Path, name: &str) -> Result<()> {
    fs::create_dir_all(migrations_dir)?;

    let snapshot_path = migrations_dir.join("schema.json");
    let old_schema = Schema::load_or_empty(&snapshot_path)?;
    let new_schema = Schema::load(schema_path)?;
    let migration = diff_schemas(&old_schema, &new_schema);

    if migration.changes.is_empty() {
        println!("No schema changes detected");
        return Ok(());
    }

    let sql = sqlite_migration_sql(&migration);
    let file_name = format!(
        "{:04}_{}.sql",
        next_migration_number(migrations_dir)?,
        slugify(name)
    );
    let migration_path = migrations_dir.join(file_name);
    fs::write(&migration_path, format!("{sql}\n"))?;
    new_schema.save(snapshot_path)?;

    println!("Created {}", migration_path.display());
    Ok(())
}

async fn migrate(database_url: Option<String>, config: &Path, migrations_dir: &Path) -> Result<()> {
    let database_url = match database_url {
        Some(database_url) => database_url,
        None => database_url_from_config(config)?,
    };
    let db = SqliteBackend::connect(&database_url).await?;
    for name in db.apply_migrations_dir(migrations_dir).await? {
        println!("Applied {name}");
    }

    Ok(())
}

fn database_url_from_config(path: &Path) -> Result<String> {
    let config = fs::read_to_string(path)?;
    let config: AppConfig = toml::from_str(&config).map_err(std::io::Error::other)?;
    Ok(config.database.url)
}

fn migration_files(migrations_dir: &Path) -> Result<Vec<PathBuf>> {
    if !migrations_dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(migrations_dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "sql") {
            files.push(path);
        }
    }
    Ok(files)
}

fn next_migration_number(migrations_dir: &Path) -> Result<u32> {
    let max = migration_files(migrations_dir)?
        .iter()
        .filter_map(|path| path.file_name()?.to_str()?.get(0..4)?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    Ok(max + 1)
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    slug.trim_matches('_').to_string()
}
