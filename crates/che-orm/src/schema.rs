use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::{FieldType, ForeignKeyAction, Model, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
/// Serializable snapshot of registered model schemas.
///
/// Snapshots are an input to migration generation and are tied to the current
/// ORM schema format.
pub struct Schema {
    pub models: Vec<ModelSchema>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Serializable schema for one database table.
pub struct ModelSchema {
    pub table: String,
    pub fields: Vec<FieldSchema>,
    #[serde(default)]
    pub indexes: Vec<IndexSchema>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Serializable modelled database index.
pub struct IndexSchema {
    pub name: String,
    pub columns: Vec<String>,
    #[serde(default)]
    pub unique: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Serializable schema for one database column.
pub struct FieldSchema {
    pub name: String,
    pub ty: FieldType,
    pub primary_key: bool,
    pub nullable: bool,
    pub auto: bool,
    pub unique: bool,
    pub max_length: Option<u32>,
    pub default: Option<String>,
    #[serde(default)]
    pub auto_now_add: bool,
    #[serde(default)]
    pub auto_now: bool,
    pub foreign_key: Option<ForeignKeySchema>,
    #[serde(default)]
    pub choices: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Serializable foreign key target and action.
pub struct ForeignKeySchema {
    pub table: String,
    #[serde(default)]
    pub on_delete: ForeignKeyAction,
}

impl Schema {
    /// Creates a schema without models.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Creates a deterministically ordered schema from model schemas.
    pub fn from_models(models: Vec<ModelSchema>) -> Self {
        let mut schema = Self { models };
        schema
            .models
            .sort_by(|left, right| left.table.cmp(&right.table));
        schema
    }

    /// Creates a schema containing one derived model.
    pub fn from_model<M: Model>() -> Self {
        Self::from_models(vec![ModelSchema::from_model::<M>()])
    }

    /// Loads a JSON schema snapshot from disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        let schema: Self = serde_json::from_str(&content)?;
        schema.validate()?;
        Ok(schema)
    }

    /// Loads a snapshot or returns an empty schema when the path is absent.
    pub fn load_or_empty(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self::empty())
        }
    }

    /// Writes a pretty-printed JSON snapshot, creating parent directories.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        self.validate()?;
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        fs::write(path, format!("{content}\n"))?;
        Ok(())
    }

    /// Validates identifiers in a schema loaded from an external snapshot.
    pub fn validate(&self) -> Result<()> {
        for model in &self.models {
            validate_identifier(&model.table)?;
            let field_names = model
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<std::collections::HashSet<_>>();
            for field in &model.fields {
                validate_identifier(&field.name)?;
                if let Some(foreign_key) = &field.foreign_key {
                    validate_identifier(&foreign_key.table)?;
                }
            }
            for index in &model.indexes {
                validate_identifier(&index.name)?;
                for column in &index.columns {
                    validate_identifier(column)?;
                    if !field_names.contains(column.as_str()) {
                        return Err(crate::Error::InvalidIdentifier(format!(
                            "index {} references unknown column {}",
                            index.name, column
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

impl ModelSchema {
    /// Derives a serializable schema from model metadata.
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
                    auto_now_add: field.auto_now_add,
                    auto_now: field.auto_now,
                    foreign_key: field.foreign_key.map(|foreign_key| ForeignKeySchema {
                        table: foreign_key.table.to_string(),
                        on_delete: foreign_key.on_delete,
                    }),
                    choices: field
                        .choices
                        .map(|choices| choices.iter().map(|choice| choice.to_string()).collect()),
                })
                .collect(),
            indexes: M::fields()
                .iter()
                .filter(|field| field.index)
                .map(|field| IndexSchema {
                    name: format!("{}_{}_idx", M::table_name(), field.db_name),
                    columns: vec![field.db_name.to_string()],
                    unique: false,
                })
                .collect(),
        }
    }
}

fn validate_identifier(identifier: &str) -> Result<()> {
    let valid = !identifier.is_empty()
        && identifier.chars().enumerate().all(|(index, character)| {
            if index == 0 {
                character.is_ascii_alphabetic() || character == '_'
            } else {
                character.is_ascii_alphanumeric() || character == '_'
            }
        });
    if valid {
        Ok(())
    } else {
        Err(crate::Error::InvalidIdentifier(identifier.to_owned()))
    }
}
