use crate::{
    invalid,
    protocol::{Record, Value},
};
use che_orm::{ColumnType, DatabaseValue, Model, OrmError, Row, TableSchema, Uuid};
use std::{collections::HashSet, sync::Arc};

/// Stable wire name mapped to a physical SQL column controlled by the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: &'static str,
    pub column: &'static str,
    pub writable: bool,
}

impl Field {
    pub const fn writable(name: &'static str, column: &'static str) -> Self {
        Self {
            name,
            column,
            writable: true,
        }
    }
    pub const fn server_owned(name: &'static str, column: &'static str) -> Self {
        Self {
            name,
            column,
            writable: false,
        }
    }
}

pub trait SyncModel: Model<PrimaryKey = Uuid> + Send + Sync + 'static {
    const SYNC_NAME: &'static str;
    const SYNC_VERSION: u32 = 1;
    /// All physical columns, including the primary key, must be mapped.
    fn sync_fields() -> Vec<Field>;
    fn registered_fields() -> Vec<Field> {
        fields::<Self>()
    }
    const REVISION_COLUMN: &'static str = "revision";
    const UPDATED_AT_COLUMN: &'static str = "updated_at";
    const DELETED_AT_COLUMN: &'static str = "deleted_at";
    const SCOPE_COLUMN: &'static str = "sync_scope";
}

pub(crate) fn fields<M: SyncModel>() -> Vec<Field> {
    let mut fields = M::sync_fields();
    for (name, column) in [
        ("revision", M::REVISION_COLUMN),
        ("updated_at", M::UPDATED_AT_COLUMN),
        ("deleted_at", M::DELETED_AT_COLUMN),
        ("sync_scope", M::SCOPE_COLUMN),
    ] {
        if !fields.iter().any(|field| field.column == column) {
            fields.push(Field::server_owned(name, column));
        }
    }
    fields
}

#[derive(Clone)]
pub(crate) struct Descriptor {
    pub name: &'static str,
    pub version: u32,
    pub schema: TableSchema,
    pub fields: Vec<Field>,
    pub primary: &'static str,
    pub read: fn(&Row) -> Result<Record, OrmError>,
    pub revision: &'static str,
    pub updated_at: &'static str,
    pub deleted_at: &'static str,
    pub scope: &'static str,
}

impl Descriptor {
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
            if let (Some(choices), Value::Text(value)) = (&column.choices, value) {
                if !choices.contains(&value.as_str()) {
                    return Err(invalid("invalid enum value"));
                }
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
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register<M: SyncModel>(mut self) -> Result<Self, OrmError> {
        let schema = M::schema();
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
        let fields = fields::<M>();
        for (name, ty, nullable) in [
            (M::REVISION_COLUMN, ColumnType::Integer, false),
            (M::UPDATED_AT_COLUMN, ColumnType::DateTime, false),
            (M::DELETED_AT_COLUMN, ColumnType::DateTime, true),
            (M::SCOPE_COLUMN, ColumnType::Text, false),
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
                    || M::write_protected_columns().contains(&column.name))
            {
                return Err(invalid(
                    "keys, managed timestamps and protected columns must be server-owned",
                ));
            }
        }
        if columns.len() != schema.columns.len() || M::SYNC_NAME.is_empty() || M::SYNC_VERSION == 0
        {
            return Err(invalid("invalid sync model mapping"));
        }
        if self
            .models
            .iter()
            .any(|d| d.name == M::SYNC_NAME || d.schema.name == schema.name)
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
            name: M::SYNC_NAME,
            version: M::SYNC_VERSION,
            schema,
            fields,
            primary,
            revision: M::REVISION_COLUMN,
            updated_at: M::UPDATED_AT_COLUMN,
            deleted_at: M::DELETED_AT_COLUMN,
            scope: M::SCOPE_COLUMN,
            read: |row| {
                let model = M::from_row(row)?;
                record::<M>(&model)
            },
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
                    {
                        if fk
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
        if d.schema != M::schema() || d.version != M::SYNC_VERSION || d.fields != fields::<M>() {
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
                .ok_or_else(|| invalid("missing materialized field"))?;
            Ok((field.name.into(), from_database(&value.value)))
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
