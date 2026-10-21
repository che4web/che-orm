//! Explicit protocol 3: principal-bound delivery of current object projections.
use crate::model_log::{ModelState, ModelWriteResult};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const OBJECT_LOG_PROTOCOL: u32 = 3;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectRights {
    pub read: bool,
    pub change: bool,
    pub delete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectCursor {
    pub model: String,
    pub history: Uuid,
    /// Opaque, persistent identity bound to the authenticated account and model.
    pub feed: Uuid,
    #[serde(with = "crate::decimal_i64")]
    pub sequence: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObjectChange {
    State {
        state: ModelState,
        rights: ObjectRights,
    },
    Revoked {
        record_id: Uuid,
    },
    Deleted {
        record_id: Uuid,
        #[serde(with = "crate::decimal_i64")]
        revision: i64,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectEvent {
    #[serde(with = "crate::decimal_i64")]
    pub sequence: i64,
    pub change: ObjectChange,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectChanges {
    pub protocol_version: u32,
    pub cursor: ObjectCursor,
    /// Exact request boundary; clients reject gaps, overlaps and stale pages.
    #[serde(with = "crate::decimal_i64")]
    pub after: i64,
    pub events: Vec<ObjectEvent>,
    pub can_create: bool,
    pub has_more: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteOutcome {
    Accepted,
    Conflict,
    Rejected,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ObjectWriteResult {
    Canonical(ModelWriteResult),
    /// The immutable receipt is unchanged. Only its outcome is disclosed.
    Acknowledged {
        operation_id: Uuid,
        outcome: WriteOutcome,
    },
}
