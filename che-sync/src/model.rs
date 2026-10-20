use crate::{
    invalid,
    protocol::{Record, Value},
};
use che_orm::{
    ColumnType, DatabaseValue, ModelRole, OrmError, Row, SchemaSet, SyncRegistration, TableSchema,
    Uuid,
};
pub use che_orm::{SyncField as Field, SyncModel};
use std::{collections::HashSet, sync::Arc};

pub(crate) fn fields<M: SyncModel>() -> Vec<Field> {
    M::registered_fields()
}

#[derive(Clone)]
pub(crate) struct Descriptor {
    pub name: &'static str,
    pub version: u32,
    pub schema: TableSchema,
    pub fields: Vec<Field>,
    pub primary: &'static str,
    pub revision: &'static str,
    pub updated_at: &'static str,
    pub deleted_at: &'static str,
    pub scope: &'static str,
    pub json_columns: Vec<&'static str>,
}

impl Descriptor {
    pub fn read_row(&self, row: &Row) -> Result<Record, OrmError> {
        let mut record = Record::new();
        for (index, column) in self.schema.columns.iter().enumerate() {
            macro_rules! value {
                ($ty:ty, $variant:ident) => {
                    row.get::<Option<$ty>>(index)?
                        .map(Value::$variant)
                        .unwrap_or(Value::Null)
                };
            }
            let value = match column.column_type {
                ColumnType::Integer => value!(i64, Integer),
                ColumnType::Real => value!(f64, Real),
                ColumnType::Text => value!(String, Text),
                ColumnType::Binary => value!(Vec<u8>, Binary),
                ColumnType::Boolean => value!(bool, Boolean),
                ColumnType::DateTime => value!(che_orm::time::OffsetDateTime, DateTime),
                ColumnType::Uuid => value!(Uuid, Uuid),
            };
            let field = self
                .fields
                .iter()
                .find(|field| field.column == column.name)
                .unwrap();
            record.insert(field.name.into(), value);
        }
        let primary = self
            .fields
            .iter()
            .find(|field| field.column == self.primary)
            .unwrap();
        let Some(Value::Uuid(id)) = record.get(primary.name) else {
            return Err(invalid("invalid materialized primary key"));
        };
        self.validate(&record, *id, true, false)?;
        Ok(record)
    }
    pub fn mirror(&self) -> String {
        format!("_sync_server_{}", self.schema.name)
    }
    pub fn field(&self, name: &str) -> Result<&Field, OrmError> {
        self.fields
            .iter()
            .find(|field| field.name == name)
            .ok_or_else(|| invalid(format!("unknown field {}.{name}", self.name)))
    }
    pub fn validate(
        &self,
        record: &Record,
        id: Uuid,
        complete: bool,
        writable: bool,
    ) -> Result<(), OrmError> {
        if complete && record.len() != self.fields.len() {
            return Err(invalid("incomplete record"));
        }
        for (name, value) in record {
            let field = self.field(name)?;
            let column = self
                .schema
                .columns
                .iter()
                .find(|column| column.name == field.column)
                .unwrap();
            if writable && !field.writable {
                return Err(invalid(format!("server-owned field {name}")));
            }
            let matches = matches!(
                (column.column_type, value),
                (ColumnType::Integer, Value::Integer(_))
                    | (ColumnType::Real, Value::Real(_))
                    | (ColumnType::Text, Value::Text(_))
                    | (ColumnType::Binary, Value::Binary(_))
                    | (ColumnType::Boolean, Value::Boolean(_))
                    | (ColumnType::DateTime, Value::DateTime(_))
                    | (ColumnType::Uuid, Value::Uuid(_))
            ) || (column.nullable && *value == Value::Null);
            if !matches {
                return Err(invalid(format!("invalid type for {name}")));
            }
            if matches!(value, Value::Real(v) if !v.is_finite()) {
                return Err(invalid("non-finite real"));
            }
            if let (Some(choices), Value::Text(value)) = (&column.choices, value)
                && !choices.contains(&value.as_str())
            {
                return Err(invalid("invalid enum value"));
            }
            if column.primary_key && *value != Value::Uuid(id) {
                return Err(invalid("primary key mismatch"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct Registry {
    pub(crate) models: Arc<Vec<Descriptor>>,
    pub(crate) role: Option<ModelRole>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }
    /// Build a registry from explicit ORM model roles. A database has one sync role.
    pub fn from_schema(schema: &SchemaSet) -> Result<Self, OrmError> {
        schema
            .validate()
            .map_err(|error| invalid(format!("{error:?}")))?;
        let mut registry = Self::new();
        for registration in schema.sync_models() {
            if registry.role.is_some_and(|role| role != registration.role) {
                return Err(invalid(
                    "a database cannot mix client and server sync roles",
                ));
            }
            registry.role = Some(registration.role);
            registry = registry.register_descriptor(registration.clone())?;
        }
        Ok(registry)
    }

    pub fn role(&self) -> Option<ModelRole> {
        self.role
    }

    pub(crate) fn require_role(&self, expected: ModelRole) -> Result<(), OrmError> {
        if self.role.is_some_and(|role| role != expected) {
            return Err(invalid("sync registry has the wrong database role"));
        }
        Ok(())
    }

    pub fn register<M: SyncModel>(self) -> Result<Self, OrmError> {
        let role = self.role.unwrap_or(ModelRole::Client);
        self.register_descriptor(SyncRegistration::new::<M>(role))
    }

    fn register_descriptor(mut self, registration: SyncRegistration) -> Result<Self, OrmError> {
        let schema = registration.schema;
        schema
            .validate()
            .map_err(|error| invalid(format!("{error:?}")))?;
        if schema.name.starts_with("_sync_") {
            return Err(invalid("reserved sync table name"));
        }
        // Names are used as SQL identifiers only after schema/ASCII validation.
        for name in std::iter::once(schema.name).chain(schema.columns.iter().map(|c| c.name)) {
            if name.is_empty()
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || name.len() > 48
            {
                return Err(invalid(
                    "sync SQL identifiers must be ASCII and at most 48 bytes",
                ));
            }
        }
        let primary = schema
            .columns
            .iter()
            .find(|c| c.primary_key)
            .ok_or_else(|| invalid("missing primary key"))?;
        if primary.column_type != ColumnType::Uuid {
            return Err(invalid("sync requires UUID keys"));
        }
        let primary = primary.name;
        let fields = registration.fields;
        for (name, ty, nullable) in [
            (registration.revision, ColumnType::Integer, false),
            (registration.updated_at, ColumnType::DateTime, false),
            (registration.deleted_at, ColumnType::DateTime, true),
            (registration.scope, ColumnType::Text, false),
        ] {
            let column = schema
                .columns
                .iter()
                .find(|column| column.name == name)
                .ok_or_else(|| invalid(format!("sync model requires column {name}")))?;
            if column.column_type != ty
                || column.nullable != nullable
                || fields
                    .iter()
                    .any(|field| field.column == name && field.writable)
            {
                return Err(invalid(format!("invalid sync metadata column {name}")));
            }
        }
        let mut columns = HashSet::new();
        let mut names = HashSet::new();
        for field in &fields {
            let column = schema
                .columns
                .iter()
                .find(|column| column.name == field.column)
                .ok_or_else(|| invalid("unknown mapped column"))?;
            if field.name.is_empty() || !columns.insert(field.column) || !names.insert(field.name) {
                return Err(invalid("duplicate or empty field mapping"));
            }
            if field.writable
                && (column.primary_key
                    || column.auto_now
                    || column.auto_now_add
                    || registration.protected_columns.contains(&column.name))
            {
                return Err(invalid(
                    "keys, managed timestamps and protected columns must be server-owned",
                ));
            }
        }
        if columns.len() != schema.columns.len()
            || registration.name.is_empty()
            || registration.version == 0
        {
            return Err(invalid("invalid sync model mapping"));
        }
        if self
            .models
            .iter()
            .any(|d| d.name == registration.name || d.schema.name == schema.name)
        {
            return Err(invalid("duplicate sync model"));
        }
        // Parents must be registered before children; cycles are rejected.
        for column in &schema.columns {
            if let Some(fk) = &column.references {
                let (table, key) = fk.target_parts().ok_or_else(|| invalid("invalid FK"))?;
                if !self
                    .models
                    .iter()
                    .any(|d| d.schema.name == table && d.primary == key)
                    || column.column_type != ColumnType::Uuid
                {
                    return Err(invalid("register UUID FK parents before children"));
                }
            }
        }
        Arc::make_mut(&mut self.models).push(Descriptor {
            name: registration.name,
            version: registration.version,
            schema,
            fields,
            primary,
            revision: registration.revision,
            updated_at: registration.updated_at,
            deleted_at: registration.deleted_at,
            scope: registration.scope,
            json_columns: registration.json_columns,
        });
        Ok(self)
    }
    pub(crate) fn get(&self, name: &str) -> Result<&Descriptor, OrmError> {
        self.models
            .iter()
            .find(|d| d.name == name)
            .ok_or_else(|| invalid(format!("unknown model {name}")))
    }
    /// Validate a typed registration without exposing internal descriptors.
    pub fn check_model<M: SyncModel>(&self) -> Result<(), OrmError> {
        self.typed::<M>().map(|_| ())
    }

    /// Predict deleted rows from a prepared operation, including cascades and
    /// respecting explicit child moves made in the same operation.
    pub fn deleted_in_operation(
        &self,
        before: &[crate::protocol::Entity],
        operation: &crate::protocol::Operation,
    ) -> Result<Vec<crate::protocol::Entity>, OrmError> {
        let mut deleted = operation
            .mutations
            .iter()
            .filter(|m| m.changes.is_none())
            .map(|m| (m.model.clone(), m.id))
            .collect::<HashSet<_>>();
        loop {
            let count = deleted.len();
            for entity in before.iter().filter(|e| e.record.is_some()) {
                let d = self.get(&entity.model)?;
                let mut record = entity.record.clone().unwrap();
                if let Some(patch) = operation
                    .mutations
                    .iter()
                    .find(|m| m.model == entity.model && m.id == entity.id)
                    .and_then(|m| m.changes.as_ref())
                {
                    record.extend(patch.clone());
                }
                for field in &d.fields {
                    let column = d
                        .schema
                        .columns
                        .iter()
                        .find(|c| c.name == field.column)
                        .unwrap();
                    if let (Some(fk), Some(crate::protocol::Value::Uuid(id))) =
                        (&column.references, record.get(field.name))
                        && fk
                            .on_delete()
                            .is_some_and(|action| action.eq_ignore_ascii_case("cascade"))
                    {
                        let parent = self
                            .models
                            .iter()
                            .find(|d| d.schema.name == fk.target_parts().unwrap().0)
                            .unwrap();
                        if deleted.contains(&(parent.name.to_owned(), *id)) {
                            deleted.insert((entity.model.clone(), entity.id));
                        }
                    }
                }
            }
            if count == deleted.len() {
                break;
            }
        }
        Ok(before
            .iter()
            .filter(|e| e.record.is_some() && deleted.contains(&(e.model.clone(), e.id)))
            .cloned()
            .collect())
    }

    pub(crate) fn feed_models(&self, model: &str) -> Result<HashSet<String>, OrmError> {
        self.get(model)?;
        let mut models = HashSet::from([model.to_owned()]);
        loop {
            let before = models.len();
            for d in self
                .models
                .iter()
                .filter(|d| models.contains(d.name))
                .collect::<Vec<_>>()
            {
                for column in &d.schema.columns {
                    if let Some(fk) = &column.references {
                        let table = fk.target_parts().unwrap().0;
                        let parent = self.models.iter().find(|d| d.schema.name == table).unwrap();
                        models.insert(parent.name.into());
                    }
                }
            }
            if models.len() == before {
                return Ok(models);
            }
        }
    }

    pub(crate) fn related_models(&self, model: &str) -> Result<HashSet<String>, OrmError> {
        let mut models = self.feed_models(model)?;
        loop {
            let before = models.len();
            for d in self.models.iter() {
                for column in &d.schema.columns {
                    if let Some(fk) = &column.references {
                        let table = fk.target_parts().unwrap().0;
                        let parent = self.models.iter().find(|d| d.schema.name == table).unwrap();
                        if models.contains(d.name) || models.contains(parent.name) {
                            models.insert(d.name.into());
                            models.insert(parent.name.into());
                        }
                    }
                }
            }
            if models.len() == before {
                return Ok(models);
            }
        }
    }
    pub(crate) fn typed<M: SyncModel>(&self) -> Result<&Descriptor, OrmError> {
        let d = self.get(M::SYNC_NAME)?;
        if d.schema != M::sync_schema() || d.version != M::SYNC_VERSION || d.fields != fields::<M>()
        {
            return Err(invalid("model registration mismatch"));
        }
        Ok(d)
    }
}

pub(crate) fn record<M: SyncModel>(model: &M) -> Result<Record, OrmError> {
    let values = model.field_values();
    fields::<M>()
        .iter()
        .map(|field| {
            let value = values
                .iter()
                .find(|v| v.column.name == field.column)
                .map(|value| from_database(&value.value));
            let value = match value {
                Some(value) => value,
                None if M::AUTOMATIC_METADATA && field.column == M::REVISION_COLUMN => {
                    Value::Integer(0)
                }
                None if M::AUTOMATIC_METADATA && field.column == M::UPDATED_AT_COLUMN => {
                    Value::DateTime(che_orm::time::OffsetDateTime::UNIX_EPOCH)
                }
                None if M::AUTOMATIC_METADATA && field.column == M::DELETED_AT_COLUMN => {
                    Value::Null
                }
                None if M::AUTOMATIC_METADATA && field.column == M::SCOPE_COLUMN => {
                    Value::Text(String::new())
                }
                None => return Err(invalid("missing materialized field")),
            };
            Ok((field.name.into(), value))
        })
        .collect()
}

pub(crate) fn from_database(value: &DatabaseValue) -> Value {
    match value {
        DatabaseValue::Integer(v) => Value::Integer(*v),
        DatabaseValue::Real(v) => Value::Real(*v),
        DatabaseValue::Text(v) => Value::Text(v.clone()),
        DatabaseValue::Binary(v) => Value::Binary(v.clone()),
        DatabaseValue::Boolean(v) => Value::Boolean(*v),
        DatabaseValue::DateTime(v) => Value::DateTime(*v),
        DatabaseValue::Uuid(v) => Value::Uuid(*v),
        DatabaseValue::Null => Value::Null,
    }
}

pub(crate) fn database(value: &Value) -> DatabaseValue {
    match value {
        Value::Integer(v) => DatabaseValue::Integer(*v),
        Value::Real(v) => DatabaseValue::Real(*v),
        Value::Text(v) => DatabaseValue::Text(v.clone()),
        Value::Binary(v) => DatabaseValue::Binary(v.clone()),
        Value::Boolean(v) => DatabaseValue::Boolean(*v),
        Value::DateTime(v) => DatabaseValue::DateTime(*v),
        Value::Uuid(v) => DatabaseValue::Uuid(*v),
        Value::Null => DatabaseValue::Null,
    }
}
