use crate::{DatabaseValue, DbEnum, Uuid};

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("invalid column index: {0}")]
    InvalidColumnIndex(usize),
    #[error("expected {expected}, got {actual}")]
    TypeMismatch {
        expected: &'static str,
        actual: &'static str,
    },
    #[error("invalid database value: {0}")]
    InvalidValue(String),
}

/// Converts a materialized database value into a model field.
pub trait FromDatabaseValue: Sized {
    fn from_database_value(value: &DatabaseValue) -> Result<Self, DecodeError>;
}

impl DatabaseValue {
    fn kind(&self) -> &'static str {
        match self {
            Self::Integer(_) => "integer",
            Self::Real(_) => "real",
            Self::Text(_) => "text",
            Self::Binary(_) => "binary",
            Self::Boolean(_) => "boolean",
            Self::DateTime(_) => "datetime",
            Self::Uuid(_) => "uuid",
            Self::Null => "null",
        }
    }
    fn mismatch(&self, expected: &'static str) -> DecodeError {
        DecodeError::TypeMismatch {
            expected,
            actual: self.kind(),
        }
    }
}

macro_rules! scalar {
    ($ty:ty, $variant:ident, $kind:literal) => {
        impl FromDatabaseValue for $ty {
            fn from_database_value(value: &DatabaseValue) -> Result<Self, DecodeError> {
                match value {
                    DatabaseValue::$variant(value) => Ok(value.clone()),
                    other => Err(other.mismatch($kind)),
                }
            }
        }
    };
}
scalar!(i64, Integer, "integer");
scalar!(String, Text, "text");
scalar!(Vec<u8>, Binary, "binary");

impl FromDatabaseValue for f64 {
    fn from_database_value(value: &DatabaseValue) -> Result<Self, DecodeError> {
        match value {
            DatabaseValue::Real(value) => Ok(*value),
            DatabaseValue::Integer(value) => Ok(*value as f64),
            other => Err(other.mismatch("real")),
        }
    }
}

impl FromDatabaseValue for bool {
    fn from_database_value(value: &DatabaseValue) -> Result<Self, DecodeError> {
        match value {
            DatabaseValue::Boolean(value) => Ok(*value),
            DatabaseValue::Integer(value) => Ok(*value != 0),
            other => Err(other.mismatch("boolean")),
        }
    }
}

impl FromDatabaseValue for Uuid {
    fn from_database_value(value: &DatabaseValue) -> Result<Self, DecodeError> {
        match value {
            DatabaseValue::Uuid(value) => Ok(*value),
            DatabaseValue::Binary(value) => Self::from_slice(value)
                .map_err(|error| DecodeError::InvalidValue(error.to_string())),
            other => Err(other.mismatch("uuid")),
        }
    }
}

impl FromDatabaseValue for time::OffsetDateTime {
    fn from_database_value(value: &DatabaseValue) -> Result<Self, DecodeError> {
        match value {
            DatabaseValue::DateTime(value) => Ok(value.to_offset(time::UtcOffset::UTC)),
            DatabaseValue::Text(value) => {
                Self::parse(value, &time::format_description::well_known::Rfc3339)
                    .map(|value| value.to_offset(time::UtcOffset::UTC))
                    .map_err(|error| DecodeError::InvalidValue(error.to_string()))
            }
            other => Err(other.mismatch("datetime")),
        }
    }
}

impl<T: FromDatabaseValue> FromDatabaseValue for Option<T> {
    fn from_database_value(value: &DatabaseValue) -> Result<Self, DecodeError> {
        if matches!(value, DatabaseValue::Null) {
            Ok(None)
        } else {
            T::from_database_value(value).map(Some)
        }
    }
}

impl<T: DbEnum> FromDatabaseValue for T {
    fn from_database_value(value: &DatabaseValue) -> Result<Self, DecodeError> {
        let text = String::from_database_value(value)?;
        T::from_str(&text)
            .ok_or_else(|| DecodeError::InvalidValue(format!("invalid enum value: {text}")))
    }
}

/// An owned row with the same decoding interface for either backend.
#[derive(Debug, Clone)]
pub struct Row {
    values: Vec<DatabaseValue>,
}

impl Row {
    pub fn new(values: Vec<DatabaseValue>) -> Self {
        Self { values }
    }
    pub fn get<T: FromDatabaseValue>(&self, index: usize) -> Result<T, DecodeError> {
        T::from_database_value(
            self.values
                .get(index)
                .ok_or(DecodeError::InvalidColumnIndex(index))?,
        )
    }
    #[cfg(feature = "sqlite")]
    pub fn from_sqlite(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        use rusqlite::types::ValueRef;
        let values = (0..row.as_ref().column_count())
            .map(|index| {
                Ok(match row.get_ref(index)? {
                    ValueRef::Null => DatabaseValue::Null,
                    ValueRef::Integer(value) => DatabaseValue::Integer(value),
                    ValueRef::Real(value) => DatabaseValue::Real(value),
                    ValueRef::Text(value) => DatabaseValue::Text(
                        std::str::from_utf8(value)
                            .map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    index,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?
                            .to_owned(),
                    ),
                    ValueRef::Blob(value) => DatabaseValue::Binary(value.to_vec()),
                })
            })
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Self::new(values))
    }
}

#[doc(hidden)]
#[cfg(feature = "sqlite")]
#[macro_export]
macro_rules! __sqlite_db_enum_adapter {
    ($name:ty) => {
        impl $crate::rusqlite::types::FromSql for $name {
            fn column_result(
                value: $crate::rusqlite::types::ValueRef<'_>,
            ) -> $crate::rusqlite::types::FromSqlResult<Self> {
                <Self as $crate::DbEnum>::from_str(value.as_str()?)
                    .ok_or($crate::rusqlite::types::FromSqlError::InvalidType)
            }
        }
    };
}

#[doc(hidden)]
#[cfg(not(feature = "sqlite"))]
#[macro_export]
macro_rules! __sqlite_db_enum_adapter {
    ($name:ty) => {};
}
