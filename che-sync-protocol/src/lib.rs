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
    Integer(#[serde(with = "decimal_i64")] i64),
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
    #[serde(with = "decimal_i64")]
    pub revision: i64,
    #[serde(default)]
    pub epoch: Option<Uuid>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub updated_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub deleted_at: Option<OffsetDateTime>,
    /// None at a positive revision is a tombstone. Revision zero with None
    /// means this UUID has never been recorded by the server.
    pub record: Option<Record>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mutation {
    pub model: String,
    pub id: Uuid,
    pub schema_version: u32,
    #[serde(with = "decimal_i64")]
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
    /// None is the whole-scope feed; Some selects a model and its FK parents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub epoch: Uuid,
    #[serde(with = "decimal_i64")]
    pub sequence: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeGroup {
    #[serde(with = "decimal_i64")]
    pub sequence: i64,
    pub entities: Vec<Entity>,
}

/// Emit decimal strings for JavaScript-safe transport. Read legacy integer
/// tokens as well so durable journals, receipts and outboxes remain readable.
mod decimal_i64 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(value: &i64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Decimal {
            String(String),
            Integer(i64),
        }
        match Decimal::deserialize(deserializer)? {
            Decimal::String(value) => value.parse().map_err(D::Error::custom),
            Decimal::Integer(value) => Ok(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_use_lossless_decimal_strings_and_read_legacy_numbers() {
        for value in [i64::MIN, i64::MAX, 9_007_199_254_740_993] {
            let encoded = serde_json::to_value(Value::Integer(value)).unwrap();
            assert_eq!(encoded["value"], value.to_string());
            assert_eq!(
                serde_json::from_value::<Value>(encoded).unwrap(),
                Value::Integer(value)
            );
        }
        assert_eq!(
            serde_json::from_str::<Value>(r#"{"type":"integer","value":42}"#).unwrap(),
            Value::Integer(42)
        );
        let cursor = Cursor {
            scope: "scope".into(),
            model: None,
            epoch: Uuid::nil(),
            sequence: i64::MAX,
        };
        let json = serde_json::to_value(&cursor).unwrap();
        assert_eq!(json["sequence"], i64::MAX.to_string());
        assert_eq!(serde_json::from_value::<Cursor>(json).unwrap(), cursor);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullRequest {
    pub protocol_version: u32,
    /// None requests the initial/recovery full load through the same endpoint.
    pub cursor: Option<Cursor>,
    /// Number of atomic groups, not individual entities.
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", content = "data", rename_all = "snake_case")]
pub enum PullResult {
    Full(Snapshot),
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
