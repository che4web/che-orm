//! `che-orm` — экспериментальная типизированная ORM для Rust.
//!
//! Backend выбирается feature `sqlite` или `postgres`: [`Database`] предоставляет async
//! pool и high-level CRUD/query facade, а [`Model`] derive генерирует metadata,
//! row decoding и insert values. SQL можно также использовать отдельно через
//! [`SqlCompiler`].
//!
//! Основной рабочий сценарий:
//!
//! ```ignore
//! use che_orm::{Database, Model};
//!
//! # #[derive(Debug, Model)]
//! # #[orm(table = "users")]
//! # struct User {
//! #     #[orm(primary_key)]
//! #     id: i64,
//! #     name: String,
//! # }
//! # #[tokio::main]
//! # async fn run() -> Result<(), che_orm::OrmError> {
//! let database = Database::connect_in_memory().await?;
//! database.create_table::<User>().await?;
//! let user = database
//!     .create::<User>()
//!     .set(User::NAME, "Alice")
//!     .execute()
//!     .await?;
//! let loaded = database.get::<User>(user.id).await?;
//! assert!(loaded.is_some());
//! # Ok(())
//! # }
//! ```
//!
//! Fixed string choices use [`DbEnum`]. It owns the JSON and database value,
//! so a variant has one canonical representation everywhere:
//!
//! ```ignore
//! # use che_orm::DbEnum;
//! #[derive(Debug, Clone, Copy, DbEnum)]
//! enum TaskStatus {
//!     Draft,
//!     #[db_enum(rename = "in_progress")]
//!     InProgress,
//! }
//! ```
//!
//! Do not derive `serde::Serialize` or `serde::Deserialize` for a `DbEnum`;
//! the derive generates both implementations.

#![allow(clippy::type_complexity, clippy::new_without_default)]

extern crate self as che_orm;

pub use che_orm_macros::{DbEnum, Model, ModelSerializer};

#[cfg(feature = "sqlite")]
pub use rusqlite;
pub use serde;
pub use serde_json;
pub use time;
#[cfg(feature = "postgres")]
pub use tokio_postgres;

#[cfg(any(feature = "sqlite", feature = "postgres"))]
mod backend;
mod image;
pub mod migration;
mod query;
mod row;
mod schema;
mod sql;
mod sync;
mod types;

#[cfg(any(feature = "sqlite", feature = "postgres"))]
mod connection;

pub use image::ImageField;
pub use migration::*;
pub use query::*;
pub use row::*;
pub use schema::*;
pub use sql::*;
pub use sync::*;
pub use types::*;
pub use uuid::Uuid;

/// Serializes a timestamp in the RFC 3339 format used by JSON API clients.
pub struct Rfc3339Timestamp<'a>(pub &'a time::OffsetDateTime);

impl serde::Serialize for Rfc3339Timestamp<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        time::serde::rfc3339::serialize(self.0, serializer)
    }
}

/// Serializes an optional timestamp in RFC 3339 when it has a value.
pub struct OptionalRfc3339Timestamp<'a>(pub &'a Option<time::OffsetDateTime>);

impl serde::Serialize for OptionalRfc3339Timestamp<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        time::serde::rfc3339::option::serialize(self.0, serializer)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode<Key = i64> {
    Create,
    Update { id: Key },
    Patch { id: Key },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationErrors {
    pub detail: String,
}

impl From<serde_json::Error> for ValidationErrors {
    fn from(error: serde_json::Error) -> Self {
        Self {
            detail: error.to_string(),
        }
    }
}

pub enum ValidatedWrite<M: Model> {
    Create(InsertQuery<M>),
    Update(UpdateQuery<M>),
}

impl<M: Model> ValidatedWrite<M> {
    pub fn set<T, V>(self, field: ModelField<M, T>, value: V) -> Self
    where
        V: QueryValue<T>,
    {
        match self {
            Self::Create(query) => Self::Create(query.set(field, value)),
            Self::Update(query) => Self::Update(query.set(field, value)),
        }
    }
}

pub trait ModelWriteSerializer {
    type Model: Model;

    fn is_valid(
        data: serde_json::Value,
        mode: WriteMode<<Self::Model as Model>::PrimaryKey>,
    ) -> Result<ValidatedWrite<Self::Model>, ValidationErrors>;
}

/// A JSON patch value. `Missing` means that the property was not sent;
/// `Value(None)` represents an explicit JSON null.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PatchField<T> {
    #[default]
    Missing,
    Value(T),
}

impl<T> PatchField<T> {
    pub const fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }
}

impl<'de, T> serde::Deserialize<'de> for PatchField<T>
where
    T: serde::Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Self::Value)
    }
}

/// Deserializes an RFC 3339 timestamp sent by JSON API clients.
pub fn deserialize_rfc3339<'de, D>(deserializer: D) -> Result<time::OffsetDateTime, D::Error>
where
    D: serde::Deserializer<'de>,
{
    time::serde::rfc3339::deserialize(deserializer)
}

/// Deserializes an optional RFC 3339 timestamp sent by JSON API clients.
pub fn deserialize_optional_rfc3339<'de, D>(
    deserializer: D,
) -> Result<Option<time::OffsetDateTime>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    time::serde::rfc3339::option::deserialize(deserializer)
}

/// Deserializes an RFC 3339 timestamp while preserving patch-field presence.
pub fn deserialize_patch_rfc3339<'de, D>(
    deserializer: D,
) -> Result<PatchField<time::OffsetDateTime>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_rfc3339(deserializer).map(PatchField::Value)
}

/// Deserializes an optional RFC 3339 timestamp while preserving patch-field presence.
pub fn deserialize_patch_optional_rfc3339<'de, D>(
    deserializer: D,
) -> Result<PatchField<Option<time::OffsetDateTime>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_optional_rfc3339(deserializer).map(PatchField::Value)
}

#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub use connection::*;

#[cfg(all(feature = "postgres", feature = "sqlite"))]
compile_error!("features `postgres` and `sqlite` are mutually exclusive");

#[cfg(not(any(feature = "postgres", feature = "sqlite")))]
compile_error!("enable exactly one database backend: `postgres` or `sqlite`");

#[cfg(test)]
mod tests;
