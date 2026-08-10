#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("model has no primary key field")]
    MissingPrimaryKey,

    #[error("update has no changed fields")]
    EmptyUpdate,

    #[error("unknown model field: {0}")]
    UnknownField(String),

    #[error("field cannot be updated: {0}")]
    ReadonlyField(String),

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

    #[error("migration checksum mismatch for {name}: expected {expected}, found {actual}")]
    MigrationChecksumMismatch {
        name: String,
        expected: String,
        actual: String,
    },

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

pub type Result<T> = std::result::Result<T, Error>;
