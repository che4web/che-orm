//! Versioned, backend-independent synchronization messages.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use time::OffsetDateTime;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 1;

/// Tagged values preserve types across JSON and between SQLite and PostgreSQL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    Integer(i64),
    Real(f64),
    Text(String),
    Binary(Vec<u8>),
    Boolean(bool),
    DateTime(OffsetDateTime),
    Uuid(Uuid),
    Null,
}

pub type Record = BTreeMap<String, Value>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub model: String,
    pub id: Uuid,
    pub schema_version: u32,
    pub revision: i64,
    /// None at a positive revision is a tombstone. Revision zero with None
    /// means this UUID has never been recorded by the server.
    pub record: Option<Record>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mutation {
    pub model: String,
    pub id: Uuid,
    pub schema_version: u32,
    pub base_revision: i64,
    /// None deletes the entity. Some contains a writable-field patch;
    /// at revision zero it creates an entity.
    pub changes: Option<Record>,
}

/// A group is atomic. Its contents must not change when retried.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    pub id: Uuid,
    pub mutations: Vec<Mutation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushRequest {
    pub protocol_version: u32,
    pub scope: String,
    pub replica_id: Uuid,
    pub operation: Operation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", content = "data", rename_all = "snake_case")]
pub enum PushResult {
    Accepted(Vec<Entity>),
    Conflict(Vec<Entity>),
    Rejected { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cursor {
    pub scope: String,
    pub epoch: Uuid,
    pub sequence: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeGroup {
    pub sequence: i64,
    pub entities: Vec<Entity>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullRequest {
    pub protocol_version: u32,
    pub cursor: Cursor,
    /// Number of atomic groups, not individual entities.
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", content = "data", rename_all = "snake_case")]
pub enum PullResult {
    Changes {
        groups: Vec<ChangeGroup>,
        cursor: Cursor,
        has_more: bool,
    },
    ResetRequired,
}

/// A single consistent snapshot. Transports may stream this message, but must
/// preserve its snapshot boundary; independent live-table pages are not safe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub entities: Vec<Entity>,
    pub cursor: Cursor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conflict {
    pub model: String,
    pub id: Uuid,
    pub base: Option<Record>,
    pub local: Option<Record>,
    pub remote: Entity,
    pub fields: Vec<String>,
    pub reason: Option<String>,
    /// Visible reconciliation was blocked by a constraint, not a competing
    /// field edit or server rejection. It may retry when dependencies are ready.
    #[serde(default)]
    pub retryable: bool,
    /// Canonical anchor for a deferred accepted write. `base` is its sent
    /// state; newer remote revisions are merged against this acknowledgement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledgement: Option<Entity>,
}
