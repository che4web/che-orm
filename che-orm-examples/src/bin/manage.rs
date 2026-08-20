use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

use che_orm_examples::{atlas_database_url, registry};
use orm::SqliteDialect;

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
        _ => Err("unknown command or arguments".into()),
    }
}

fn migrations_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations")
}

fn migration_url() -> String {
    format!("file://{}", migrations_dir().display())
}

fn desired_schema() -> String {
    registry().to_sql::<SqliteDialect>()
}

fn print_schema() -> Result<(), String> {
    print!("{}", desired_schema());
    Ok(())
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

fn generated_migration_name() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    format!("auto_{timestamp}")
}

fn migrate_diff(name: &str) -> Result<(), String> {
    let schema_file = TemporarySchema::create(&desired_schema())?;
    let directory = migration_url();
    run_atlas([
        "migrate",
        "diff",
        name,
        "--dir",
        &directory,
        "--to",
        &schema_file.url(),
        "--dev-url",
        "sqlite://dev?mode=memory",
    ])
}

fn migrate_command(args: &[String]) -> Result<(), String> {
    match args {
        [] => migrate_apply(&[]),
        [command, rest @ ..] if command == "status" => migrate_status(rest),
        [command, rest @ ..] if command == "lint" => migrate_lint(rest),
        [command, rest @ ..] if command == "apply" => migrate_apply(rest),
        _ => Err("unknown migrate command".into()),
    }
}

fn migrate_apply(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err("migrate uses the example database settings; unexpected arguments".into());
    }
    let directory = migration_url();
    let database_url = atlas_database_url()?;
    run_atlas([
        "migrate",
        "apply",
        "--dir",
        &directory,
        "--url",
        &database_url,
    ])
}

fn migrate_status(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err(
            "migrate status uses the example database settings; unexpected arguments".into(),
        );
    }
    let directory = migration_url();
    let database_url = atlas_database_url()?;
    run_atlas([
        "migrate",
        "status",
        "--dir",
        &directory,
        "--url",
        &database_url,
    ])
}

fn migrate_lint(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err("migrate lint does not accept arguments".into());
    }
    let directory = migration_url();
    run_atlas([
        "migrate",
        "lint",
        "--dir",
        &directory,
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
            "che_orm_schema_{}_{}.sql",
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
        "Usage:\n  cargo run -p che-orm-examples --bin manage -- schema\n  cargo run -p che-orm-examples --bin manage -- makemigrations [name]\n  cargo run -p che-orm-examples --bin manage -- migrate\n  cargo run -p che-orm-examples --bin manage -- migrate status\n  cargo run -p che-orm-examples --bin manage -- migrate lint\n\nDatabase:\n  che-orm-examples/src/lib.rs\n\nEnvironment:\n  ATLAS_BIN  Atlas executable path (default: atlas)"
    );
}
