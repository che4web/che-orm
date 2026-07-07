use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::{FieldType, Model, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Schema {
    pub models: Vec<ModelSchema>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSchema {
    pub table: String,
    pub fields: Vec<FieldSchema>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldSchema {
    pub name: String,
    pub ty: FieldType,
    pub primary_key: bool,
    pub nullable: bool,
    pub auto: bool,
    pub unique: bool,
    pub max_length: Option<u32>,
    pub default: Option<String>,
    pub foreign_key: Option<ForeignKeySchema>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKeySchema {
    pub table: String,
    pub column: String,
}

impl Schema {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_models(models: Vec<ModelSchema>) -> Self {
        let mut schema = Self { models };
        schema
            .models
            .sort_by(|left, right| left.table.cmp(&right.table));
        schema
    }

    pub fn from_model<M: Model>() -> Self {
        Self::from_models(vec![ModelSchema::from_model::<M>()])
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }

    pub fn load_or_empty(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self::empty())
        }
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        fs::write(path, format!("{content}\n"))?;
        Ok(())
    }
}

impl ModelSchema {
    pub fn from_model<M: Model>() -> Self {
        Self {
            table: M::table_name().to_string(),
            fields: M::fields()
                .iter()
                .map(|field| FieldSchema {
                    name: field.db_name.to_string(),
                    ty: field.ty,
                    primary_key: field.primary_key,
                    nullable: field.nullable,
                    auto: field.auto,
                    unique: field.unique,
                    max_length: field.max_length,
                    default: field.default.map(str::to_string),
                    foreign_key: field.foreign_key.map(|foreign_key| ForeignKeySchema {
                        table: foreign_key.table.to_string(),
                        column: foreign_key.column.to_string(),
                    }),
                })
                .collect(),
        }
    }
}
