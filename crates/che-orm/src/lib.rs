pub use che_orm_macros::Model;

pub mod error;
pub mod manager;
pub mod migration;
pub mod model;
pub mod schema;
pub mod sqlite;

pub use error::{Error, Result};
pub use manager::{ModelManager, UpdateBuilder};
pub use migration::{
    Migration, SchemaChange, create_table_sql, diff_schemas, sqlite_migration_sql,
};
pub use model::{FieldInfo, FieldType, ForeignKeyInfo, Model, SqliteModel, SqliteValue};
pub use schema::{FieldSchema, ForeignKeySchema, ModelSchema, Schema};
pub use sqlite::SqliteBackend;

#[doc(hidden)]
pub mod __private {
    pub use sqlx;
}
