use crate::{ColumnType, ColumnTypeOf, DatabaseValue, QueryValue, rusqlite, serde};

/// A relative storage key for an image. The bytes live in a media backend,
/// not in the database; `che-rest` manages uploads and URLs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageField(String);

impl ImageField {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub fn key(&self) -> &str {
        &self.0
    }
}

impl ColumnTypeOf for ImageField {
    fn column_type() -> ColumnType {
        ColumnType::Text
    }
}

impl QueryValue<ImageField> for ImageField {
    fn into_query_value(self) -> DatabaseValue {
        DatabaseValue::Text(self.0)
    }
}

impl rusqlite::types::ToSql for ImageField {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.0.as_str().into())
    }
}

impl rusqlite::types::FromSql for ImageField {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        Ok(Self(value.as_str()?.to_owned()))
    }
}

impl serde::Serialize for ImageField {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}
