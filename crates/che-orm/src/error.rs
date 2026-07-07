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
}

pub type Result<T> = std::result::Result<T, Error>;
