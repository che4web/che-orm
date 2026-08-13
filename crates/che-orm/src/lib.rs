//! # che-orm
//!
//! Типизированный ORM с моделями, CRUD, запросами и миграциями. Выберите ровно
//! один backend: SQLite включён по умолчанию, PostgreSQL включается feature
//! `postgres` при отключённых default features.
//!
//! ```toml
//! che-orm = "0.1" # SQLite
//! # che-orm = { version = "0.1", default-features = false, features = ["postgres"] }
//! ```
//!
//! Основная точка входа - [`Database`]. `#[derive(Model)]` создаёт реализацию
//! [`Model`] и типизированные константы `ModelNameFields` для CRUD и запросов.
//! Relations, signals, projections, группировки и числовые агрегаты доступны
//! только с SQLite. Подробные руководства доступны в README репозитория.

#![doc = include_str!("../README.md")]

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

/// Application registry, settings, and runtime migration manager.
pub mod application;
/// Backend-neutral database facade, CRUD builders, and migration operations.
pub mod database;
/// Error and result types returned by this crate.
pub mod error;
/// Validated file paths and local file storage.
pub mod files;
#[cfg(feature = "sqlite")]
pub mod manager;
/// Schema diffing and SQL generation helpers.
pub mod migration;
/// Model metadata, typed field descriptors, and database values.
pub mod model;
#[cfg(feature = "postgres")]
pub mod postgres;
/// Django-style predicate composition with [`Q`].
pub mod query;
#[cfg(feature = "sqlite")]
pub mod relation;
/// Serializable model schema snapshots.
pub mod schema;
/// Best-effort model lifecycle events.
pub mod signals;
#[cfg(feature = "sqlite")]
pub mod sqlite;

pub use application::{Application, DatabaseSettings, Manager, MigrationSettings, RuntimeSettings};
#[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
pub use database::generate_migrations;
#[cfg(feature = "migration-atlas")]
pub use database::{AtlasOptions, MigrationDialect};
pub use database::{Database, DatabaseCreateBuilder, DatabaseUpdateBuilder, MigrationStatus};
#[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
pub use database::{GeneratedMigration, MigrationOptions};
pub use error::{Error, Result};
pub use files::{FilePath, FileStorage, LocalFileStorage};
#[cfg(feature = "sqlite")]
pub use manager::{
    AnnotationField, AnnotationPredicate, GroupProjectionSpec, NumericQueryField,
    OptionalProjectionField, OptionalProjectionValue, PrefetchQuery, Prefetched, ProjectionQuery,
    ProjectionSpec, ProjectionValue, QueryBuilder, QueryField, SelectRelatedPairQuery,
    SelectRelatedQuery, TextQueryField, TypedProjectionQuery, TypedQueryField,
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
    ModelField, QueryValue, WriteValue,
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
