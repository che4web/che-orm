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

extern crate self as che_orm2;

pub use che_orm2_macros::{Model, ModelSerializer};

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

/// A JSON patch value. `Missing` means that the property was not sent;
/// `Value(None)` represents an explicit JSON null.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchField<T> {
    Missing,
    Value(T),
}

impl<T> Default for PatchField<T> {
    fn default() -> Self {
        Self::Missing
    }
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
