use crate::{ColumnType, ColumnTypeOf, DatabaseValue, DecodeError, FromDatabaseValue, QueryValue};

/// JSON stored as SQL TEXT and serialized as a JSON value, never as an escaped string.
#[derive(Debug, Clone, PartialEq, crate::serde::Serialize, crate::serde::Deserialize)]
#[serde(transparent)]
pub struct JsonField(pub crate::serde_json::Value);

impl ColumnTypeOf for JsonField {
    fn column_type() -> ColumnType {
        ColumnType::Text
    }
}

impl QueryValue<JsonField> for JsonField {
    fn into_query_value(self) -> DatabaseValue {
        DatabaseValue::Text(self.0.to_string())
    }
}

impl FromDatabaseValue for JsonField {
    fn from_database_value(value: &DatabaseValue) -> Result<Self, DecodeError> {
        let text = String::from_database_value(value)?;
        crate::serde_json::from_str(&text)
            .map(Self)
            .map_err(|error| DecodeError::InvalidValue(error.to_string()))
    }
}

#[cfg(feature = "sqlite")]
impl crate::rusqlite::types::ToSql for JsonField {
    fn to_sql(&self) -> crate::rusqlite::Result<crate::rusqlite::types::ToSqlOutput<'_>> {
        Ok(crate::rusqlite::types::ToSqlOutput::Owned(
            self.0.to_string().into(),
        ))
    }
}

#[cfg(feature = "sqlite")]
impl crate::rusqlite::types::FromSql for JsonField {
    fn column_result(
        value: crate::rusqlite::types::ValueRef<'_>,
    ) -> crate::rusqlite::types::FromSqlResult<Self> {
        crate::serde_json::from_str(value.as_str()?)
            .map(Self)
            .map_err(|error| crate::rusqlite::types::FromSqlError::Other(Box::new(error)))
    }
}
