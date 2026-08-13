#[derive(Debug, thiserror::Error)]
/// Errors returned by database, schema, migration, and file operations.
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("configuration error: {0}")]
    Config(#[from] toml::de::Error),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    #[error("Atlas executable '{binary}' could not be started: {source}")]
    AtlasUnavailable {
        binary: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Atlas migration generation failed: {details}")]
    AtlasFailed { details: String },

    #[error("model has no primary key field")]
    MissingPrimaryKey,

    #[error("model row was not found")]
    NotFound,

    #[error("update has no changed fields")]
    EmptyUpdate,

    #[error("unknown model field: {0}")]
    UnknownField(String),

    #[error("invalid SQL identifier: {0}")]
    InvalidIdentifier(String),

    #[error("field cannot be updated: {0}")]
    ReadonlyField(String),

    #[error("invalid value for field {field}, expected {expected}")]
    InvalidFieldValue {
        field: String,
        expected: &'static str,
    },

    #[error("invalid file path: {0}")]
    InvalidFilePath(String),

    #[error("invalid file extension: {0}")]
    InvalidFileExtension(String),

    #[error("file storage error: {0}")]
    Storage(String),

    #[error("unsafe migration: {0}")]
    UnsafeMigration(String),

    #[error("invalid aggregate field: {0}")]
    InvalidAggregateField(String),

    #[error("invalid relation: {0}")]
    InvalidRelation(String),

    #[error("projection decode failed: {0}")]
    ProjectionDecode(String),

    #[error("invalid annotation: {0}")]
    InvalidAnnotation(String),

    #[error("foreign key check failed: {0}")]
    ForeignKeyCheckFailed(String),

    #[error("migration preflight failed for {rule}: {details}")]
    MigrationPreflightFailed { rule: String, details: String },

    #[error(
        "migration failed ({original}) and foreign key enforcement could not be restored: {restore}"
    )]
    ForeignKeyRestoreFailed {
        original: String,
        #[source]
        restore: sqlx::Error,
    },

    #[error("migration committed, but foreign key enforcement could not be restored: {0}")]
    ForeignKeyEnforcementRestoreFailed(#[source] sqlx::Error),
}

/// Convenient result alias used by the public API.
pub type Result<T> = std::result::Result<T, Error>;
