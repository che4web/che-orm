//! Durable current/shadow synchronization for UUID-keyed che-orm models.
//! See `docs/sync.md` in the repository for the storage and access contract.
mod client;
mod engine;
mod merge;
mod model;
mod runtime;
mod schema;
mod server;
mod storage;

pub use che_sync_protocol as protocol;
pub use client::{Resolution, SyncClient, SyncTransaction};
pub use engine::{LoadRequired, SyncEngine, SyncReport, SyncTransport, TransportFuture};
pub use model::{Field, Registry, SyncModel};
pub use schema::SchemaRole;
pub use server::{AccessPolicy, AllowAll, PostCommitEffect, PreparedEffect, SyncServer};

use che_orm::OrmError;

pub(crate) fn invalid(message: impl Into<String>) -> OrmError {
    OrmError::Configuration(message.into())
}

pub(crate) fn encode<T: serde::Serialize>(value: &T) -> Result<String, OrmError> {
    serde_json::to_string(value).map_err(|error| invalid(error.to_string()))
}

pub(crate) fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, OrmError> {
    serde_json::from_str(value).map_err(|error| invalid(error.to_string()))
}
