pub use che_orm_macros::{Choice, Model};
pub use chrono;
pub use chrono::NaiveDateTime;

#[cfg(all(feature = "sqlite", feature = "postgres"))]
compile_error!("enable exactly one che-orm backend feature");

#[cfg(not(any(feature = "sqlite", feature = "postgres")))]
compile_error!("enable either the sqlite or postgres che-orm backend feature");

#[cfg(all(feature = "migration-native", feature = "migration-atlas"))]
compile_error!("enable at most one che-orm migration authoring feature");

#[cfg(all(feature = "migration-native", not(feature = "sqlite")))]
compile_error!("migration-native requires the sqlite che-orm backend feature");

pub mod application;
pub mod database;
pub mod error;
pub mod files;
#[cfg(feature = "sqlite")]
pub mod manager;
pub mod migration;
pub mod model;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod query;
#[cfg(feature = "sqlite")]
pub mod relation;
pub mod schema;
pub mod signals;
#[cfg(feature = "sqlite")]
pub mod sqlite;

pub use application::{Application, DatabaseSettings, Manager, MigrationSettings, RuntimeSettings};
#[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
pub use database::generate_migrations;
#[cfg(feature = "migration-atlas")]
pub use database::{AtlasOptions, MigrationDialect};
pub use database::{Database, DatabaseCreateBuilder, MigrationStatus};
#[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
pub use database::{GeneratedMigration, MigrationOptions};
pub use error::{Error, Result};
pub use files::{FilePath, FileStorage, LocalFileStorage};
#[cfg(feature = "sqlite")]
pub use manager::{
    AnnotationField, AnnotationPredicate, GroupProjectionSpec, NumericQueryField,
    OptionalProjectionField, OptionalProjectionValue, PrefetchQuery, Prefetched, ProjectionQuery,
    ProjectionSpec, ProjectionValue, QueryBuilder, QueryField, SelectRelatedQuery, TextQueryField,
    TypedProjectionQuery, TypedQueryField, UpdateBuilder,
};
pub use migration::{
    Migration, SchemaChange, create_table_sql, diff_schemas, postgres_schema_sql,
    sqlite_migration_sql, sqlite_schema_sql, validate_migration,
};
#[cfg(feature = "postgres")]
pub use model::PostgresModel;
#[cfg(feature = "sqlite")]
pub use model::SqliteModel;
#[cfg(feature = "sqlite")]
pub use model::{AggregateValue, SqliteValue};
pub use model::{
    Choice, DatabaseValue, FieldInfo, FieldType, ForeignKeyAction, ForeignKeyInfo, Model,
    ModelField, QueryValue,
};
#[cfg(feature = "postgres")]
pub use postgres::{PostgresBackend, PostgresQueryBuilder};
pub use query::{ContainsQueryValue, Q};
#[cfg(feature = "sqlite")]
pub use relation::{BelongsTo, HasMany};
pub use schema::{FieldSchema, ForeignKeySchema, IndexSchema, ModelSchema, Schema};
pub use signals::{ModelEvent, PostSaveEvent, PostUpdateEvent, Signals};
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteBackend;

#[doc(hidden)]
pub mod __private {
    pub use chrono;
    pub use serde_json;
    pub use sqlx;
}
