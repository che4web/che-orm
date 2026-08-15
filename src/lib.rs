extern crate self as che_orm2;

pub use che_orm2_macros::Model;

pub use rusqlite;

mod query;
mod schema;
mod sql;
mod types;

#[cfg(feature = "sqlite")]
mod connection;

pub mod models;

pub use query::*;
pub use schema::*;
pub use sql::*;
pub use types::*;

#[cfg(feature = "sqlite")]
pub use connection::*;

#[cfg(all(feature = "postgres", feature = "sqlite"))]
compile_error!("features `postgres` and `sqlite` are mutually exclusive");

#[cfg(not(any(feature = "postgres", feature = "sqlite")))]
compile_error!("enable exactly one database backend: `postgres` or `sqlite`");

#[cfg(test)]
mod tests;
