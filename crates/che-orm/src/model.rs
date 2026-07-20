use crate::ModelManager;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Integer,
    Text,
    Boolean,
    Real,
}

#[derive(Debug, Clone, Copy)]
pub struct ForeignKeyInfo {
    pub table: &'static str,
    pub column: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct FieldInfo {
    pub rust_name: &'static str,
    pub db_name: &'static str,
    pub ty: FieldType,
    pub primary_key: bool,
    pub nullable: bool,
    pub auto: bool,
    pub unique: bool,
    pub max_length: Option<u32>,
    pub default: Option<&'static str>,
    pub foreign_key: Option<ForeignKeyInfo>,
}

#[derive(Debug, Clone)]
pub enum SqliteValue {
    I64(i64),
    String(String),
    Bool(bool),
    F64(f64),
    Null,
}

impl From<i64> for SqliteValue {
    fn from(value: i64) -> Self {
        Self::I64(value)
    }
}

impl From<i32> for SqliteValue {
    fn from(value: i32) -> Self {
        Self::I64(value.into())
    }
}

impl From<u32> for SqliteValue {
    fn from(value: u32) -> Self {
        Self::I64(value.into())
    }
}

impl From<String> for SqliteValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for SqliteValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_string())
    }
}

impl From<bool> for SqliteValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<f64> for SqliteValue {
    fn from(value: f64) -> Self {
        Self::F64(value)
    }
}

impl From<f32> for SqliteValue {
    fn from(value: f32) -> Self {
        Self::F64(value.into())
    }
}

pub trait Model: Sized + Send + Sync + 'static {
    type Id: Clone + Send + Sync + for<'q> sqlx::Encode<'q, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>;
    type Update: Send + Sync;

    fn table_name() -> &'static str;
    fn fields() -> &'static [FieldInfo];

    fn primary_key() -> Option<&'static FieldInfo> {
        Self::fields().iter().find(|field| field.primary_key)
    }

    fn objects(db: &crate::SqliteBackend) -> ModelManager<'_, Self>
    where
        Self: SqliteModel,
    {
        ModelManager::new(db)
    }

    fn get_value(&self, _field: &str) -> Option<crate::__private::serde_json::Value> {
        None
    }
}

pub trait SqliteModel: Model {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> sqlx::Result<Self>;
    fn id(&self) -> Self::Id;
    fn update_values(data: Self::Update) -> Vec<(&'static str, SqliteValue)>;
    fn save_values(&self) -> Vec<(&'static str, SqliteValue)>;
}
