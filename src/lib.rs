//! `che-orm2` — экспериментальная типизированная ORM для Rust.
//!
//! Runtime сейчас ориентирован на SQLite: [`Database`] предоставляет async
//! pool и high-level CRUD/query facade, а [`Model`] derive генерирует metadata,
//! row decoding и insert values. SQL можно также использовать отдельно через
//! [`SqlCompiler`].
//!
//! Основной рабочий сценарий:
//!
//! ```ignore
//! use che_orm2::{Database, Model};
//!
//! # #[derive(Debug, Model)]
//! # #[orm(table = "users")]
//! # struct User {
//! #     #[orm(primary_key)]
//! #     id: i64,
//! #     name: String,
//! # }
//! # #[tokio::main]
//! # async fn run() -> Result<(), che_orm2::OrmError> {
//! let database = Database::connect_in_memory()?;
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
//! # use che_orm2::DbEnum;
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

extern crate self as che_orm2;

pub use che_orm2_macros::{DbEnum, Model, ModelSerializer};

pub use rusqlite;
pub use serde;
pub use serde_json;
pub use time;

mod query;
mod schema;
mod sql;
mod types;

#[cfg(feature = "sqlite")]
mod connection;

pub mod apps;
pub mod models;
pub mod settings;

pub use query::*;
pub use schema::*;
pub use sql::*;
pub use types::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    Create,
    Update { id: i64 },
    Patch { id: i64 },
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
        mode: WriteMode,
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

#[cfg(feature = "sqlite")]
pub use connection::*;

#[cfg(all(feature = "postgres", feature = "sqlite"))]
compile_error!("features `postgres` and `sqlite` are mutually exclusive");

#[cfg(not(any(feature = "postgres", feature = "sqlite")))]
compile_error!("enable exactly one database backend: `postgres` or `sqlite`");

#[cfg(test)]
mod tests;
