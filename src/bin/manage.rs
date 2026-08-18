use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

use che_orm2::apps::registry;
use che_orm2::{SqliteDialect, settings};

const MIGRATIONS_DIR: &str = "migrations";

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
        [command] if command == "schema" => print_schema(),
        [command, rest @ ..] if command == "makemigrations" => makemigrations(rest),
        [migrate, action, name] if migrate == "migrate" && action == "diff" => migrate_diff(name),
        [migrate] if migrate == "migrate" => migrate_apply(&[]),
        [migrate, rest @ ..] if migrate == "migrate" => migrate_command(rest),
        _ => {
            print_help();
            Err("unknown command or arguments".into())
        }
    }
}

fn makemigrations(args: &[String]) -> Result<(), String> {
    let name = match args {
        [] => generated_migration_name(),
        [name] => name.clone(),
        _ => return Err("makemigrations accepts zero or one name".into()),
    };
    println!("Generating migration: {name}");
    migrate_diff(&name)
}

fn migrate_command(args: &[String]) -> Result<(), String> {
    let Some(command) = args.first() else {
        return migrate_apply(&[]);
    };

    match command.as_str() {
        "status" => migrate_status(&args[1..]),
        "lint" => migrate_lint(&args[1..]),
        "apply" => migrate_apply(&args[1..]),
        _ => Err(format!("unknown migrate command: {command}")),
    }
}

fn desired_schema() -> String {
    registry().to_sql::<SqliteDialect>()
}

fn print_schema() -> Result<(), String> {
    print!("{}", desired_schema());
    Ok(())
}

fn migrate_diff(name: &str) -> Result<(), String> {
    let schema_file = TemporarySchema::create(&desired_schema())?;
    run_atlas([
        "migrate",
        "diff",
        name,
        "--dir",
        &format!("file://{MIGRATIONS_DIR}"),
        "--to",
        &schema_file.url(),
        "--dev-url",
        "sqlite://dev?mode=memory",
    ])
}

fn generated_migration_name() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    format!("auto_{timestamp}")
}

fn migrate_apply(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err("migrate uses database settings; unexpected arguments".into());
    }
    let url = settings::atlas_database_url()?;
    run_atlas([
        "migrate",
        "apply",
        "--dir",
        &format!("file://{MIGRATIONS_DIR}"),
        "--url",
        &url,
    ])
}

fn migrate_status(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err("migrate status uses database settings; unexpected arguments".into());
    }
    let url = settings::atlas_database_url()?;
    run_atlas([
        "migrate",
        "status",
        "--dir",
        &format!("file://{MIGRATIONS_DIR}"),
        "--url",
        &url,
    ])
}

fn migrate_lint(_args: &[String]) -> Result<(), String> {
    run_atlas([
        "migrate",
        "lint",
        "--dir",
        &format!("file://{MIGRATIONS_DIR}"),
        "--dev-url",
        "sqlite://dev?mode=memory",
    ])
}

fn run_atlas<const N: usize>(args: [&str; N]) -> Result<(), String> {
    let binary = env::var("ATLAS_BIN").unwrap_or_else(|_| "atlas".into());
    let status = Command::new(&binary)
        .args(args)
        .status()
        .map_err(|error| format!("cannot start {binary}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{binary} exited with status {status}"))
    }
}

struct TemporarySchema {
    path: PathBuf,
}

impl TemporarySchema {
    fn create(sql: &str) -> Result<Self, String> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "che_orm2_schema_{}_{}.sql",
            std::process::id(),
            timestamp
        ));
        fs::write(&path, sql)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        Ok(Self { path })
    }

    fn url(&self) -> String {
        format!("file://{}", self.path.display())
    }
}

impl Drop for TemporarySchema {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn print_help() {
    println!(
        "Usage:\n  cargo run --bin manage -- schema\n  cargo run --bin manage -- makemigrations [name]\n  cargo run --bin manage -- migrate\n  cargo run --bin manage -- migrate status\n  cargo run --bin manage -- migrate lint\n\nLegacy commands:\n  cargo run --bin manage -- migrate diff <name>\n\nDatabase:\n  configured in src/settings.rs\n\nEnvironment:\n  ATLAS_BIN  Atlas executable path (default: atlas)"
    );
}
