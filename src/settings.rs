use std::path::Path;

/// Application SQLite database path used by the runtime and `manage` CLI.
pub const DATABASE_PATH: &str = "app.db";

/// Returns the configured application database path.
pub const fn database_path() -> &'static str {
    DATABASE_PATH
}

/// Converts the application path into an Atlas SQLite URL.
pub fn atlas_database_url() -> Result<String, String> {
    let path = database_path();
    if path.is_empty() {
        return Err("database path must not be empty".into());
    }
    if path == ":memory:" {
        return Err("in-memory SQLite cannot be used for Atlas migrations".into());
    }
    if Path::new(path).is_dir() {
        return Err(format!("database path points to a directory: {path}"));
    }
    Ok(format!("sqlite://{path}"))
}
