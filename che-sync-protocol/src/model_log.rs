//! Transport-independent per-model journals and mutation results.
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use uuid::Uuid;

pub const MODEL_LOG_PROTOCOL: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Create,
    Update,
    Delete,
}

fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// Missing and explicit JSON null are different. `*_sql_null` also distinguishes
/// SQL NULL from JSON null for nullable JSON columns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldDiff {
    pub field: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub old_value: Option<Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub new_value: Option<Value>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub old_sql_null: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub new_sql_null: bool,
}
fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelOperation {
    #[serde(with = "crate::decimal_i64")]
    pub id: i64,
    pub record_id: Uuid,
    #[serde(with = "crate::decimal_i64")]
    pub revision: i64,
    pub kind: ChangeKind,
    pub diff: Vec<FieldDiff>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub schema_version: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelChanges {
    pub model: String,
    /// Detect history replacement; it is not a workspace or a cursor.
    pub history: Uuid,
    pub operations: Vec<ModelOperation>,
    #[serde(with = "crate::decimal_i64")]
    pub next_cursor: i64,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelState {
    pub model: String,
    pub record_id: Uuid,
    #[serde(with = "crate::decimal_i64")]
    pub revision: i64,
    pub schema_version: u32,
    pub values: Option<Map<String, Value>>,
    #[serde(default)]
    pub sql_null_fields: Vec<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub updated_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub deleted_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", content = "data", rename_all = "snake_case")]
pub enum ModelWriteResult {
    Accepted(Vec<ModelState>),
    Conflict(Vec<ModelState>),
    Rejected { code: String, reason: String },
}

/// An immutable model mutation independent of its delivery transport.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelWrite {
    pub model: String,
    pub record_id: Uuid,
    pub operation_id: Uuid,
    pub replica_id: Uuid,
    #[serde(with = "crate::decimal_i64")]
    pub base_revision: i64,
    pub schema_version: u32,
    pub kind: ChangeKind,
    pub changes: Option<Map<String, Value>>,
    #[serde(default)]
    pub sql_null_fields: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diff_distinguishes_missing_null_and_json_types() {
        let input = serde_json::json!([
            {"field":"optional", "new_value":null},
            {"field":"document", "old_value":{"x":[1,true,null]}, "new_value":"text"},
            {"field":"removed", "old_value":null, "old_sql_null":true}
        ]);
        let diff: Vec<FieldDiff> = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(diff[0].old_value, None);
        assert_eq!(diff[0].new_value, Some(Value::Null));
        assert_eq!(diff[2].new_value, None);
        assert_eq!(serde_json::to_value(diff).unwrap(), input);
    }
}
