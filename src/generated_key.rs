//! SQLite display keys backed by a counter per parent and record kind.
use crate::SchemaRequirement as Req;
use crate::migration::quote_identifier as quote;
use crate::trigger::registered_column;
use crate::{
    ColumnRef, ColumnType, MigrationError, Model, ModelField, ProjectState, SchemaObjectKind,
    SchemaObjectState, SqlDialect, SqliteDialect,
};
use std::collections::BTreeSet;
use std::marker::PhantomData;

/// Declares a shared counter table keyed by the parent ID and record kind.
pub struct SqliteKeySequence<P> {
    pub(crate) definition: KeySequenceDefinition,
    parent: PhantomData<fn() -> P>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeySequenceDefinition {
    pub(crate) name: String,
    pub(crate) parent_id: ColumnRef,
    pub(crate) parent_model: &'static str,
    parent_key: ColumnRef,
    legacy_integer_affinity: bool,
}

impl<P: Model> SqliteKeySequence<P> {
    pub fn new(name: impl Into<String>, parent_key: &ModelField<P, String>) -> Self {
        Self {
            definition: KeySequenceDefinition {
                name: name.into(),
                parent_id: P::primary_key().column(),
                parent_model: P::model_name(),
                parent_key: parent_key.column(),
                legacy_integer_affinity: false,
            },
            parent: PhantomData,
        }
    }

    /// Reuses the original unquoted counter-table declaration for applications
    /// that already store UUID project IDs in an INTEGER-affinity column.
    /// SQLite stores those UUID values as blobs despite the declared affinity.
    pub fn with_legacy_integer_affinity(mut self) -> Self {
        self.definition.legacy_integer_affinity = true;
        self
    }
}

impl KeySequenceDefinition {
    pub(crate) fn compile(
        &self,
        app: &str,
        state: &ProjectState,
    ) -> Result<SchemaObjectState, MigrationError> {
        let parent_id = registered_column(state, self.parent_id)?;
        let parent_key = registered_column(state, self.parent_key)?;
        if self.parent_id.table != self.parent_key.table
            || !parent_id.primary_key
            || parent_key.column_type != ColumnType::Text
            || parent_key.nullable
            || !parent_key.unique
        {
            return Err(invalid(
                "sequence requires a parent primary key and a unique, required text prefix",
            ));
        }
        crate::schema::validate_table_name(&self.name)
            .map_err(|_| invalid("invalid sequence table name"))?;
        let mut id_type = String::new();
        let (name, table, id) = if self.legacy_integer_affinity {
            for identifier in [self.parent_id.table, self.parent_id.name] {
                crate::schema::validate_table_name(identifier)
                    .map_err(|_| invalid("legacy sequence requires simple parent identifiers"))?;
            }
            if !matches!(
                parent_id.column_type,
                ColumnType::Uuid | ColumnType::Integer
            ) {
                return Err(invalid(
                    "legacy sequence requires a UUID or integer parent key",
                ));
            }
            id_type.push_str("INTEGER");
            (
                self.name.clone(),
                self.parent_id.table.into(),
                self.parent_id.name.into(),
            )
        } else {
            SqliteDialect::column_type(parent_id.column_type, &mut id_type);
            (
                quote(&self.name),
                quote(self.parent_id.table),
                quote(self.parent_id.name),
            )
        };
        let sql = format!(
            "CREATE TABLE {name} (project_id {id_type} NOT NULL REFERENCES {table}({id}) ON DELETE CASCADE, kind TEXT NOT NULL, next_value INTEGER NOT NULL, PRIMARY KEY (project_id, kind))"
        );
        Ok(SchemaObjectState {
            app: app.into(),
            kind: SchemaObjectKind::Table,
            name: self.name.clone(),
            sql,
        })
    }
}

/// Generates keys such as `PROJ-TASK-0001` from a parent prefix and a shared
/// SQLite sequence. Inserts use the empty key default, and the assigned key
/// is available when the row is read after the insert. For synchronized
/// models, register this definition on the server and mark the key field
/// `server_owned` so clients receive the assigned value.
pub struct SqliteGeneratedKey<M> {
    pub(crate) definition: GeneratedKeyDefinition,
    model: PhantomData<fn() -> M>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GeneratedKeyDefinition {
    pub(crate) model_name: &'static str,
    pub(crate) key: ColumnRef,
    parent: ColumnRef,
    id: ColumnRef,
    kind: String,
    pub(crate) sequence: KeySequenceDefinition,
}

impl<M: Model> SqliteGeneratedKey<M> {
    /// The parent field and sequence must use the same primary key type.
    pub fn new<P: Model>(
        key: &ModelField<M, String>,
        parent: &ModelField<M, P::PrimaryKey>,
        sequence: &SqliteKeySequence<P>,
        kind: impl Into<String>,
    ) -> Self {
        Self {
            definition: GeneratedKeyDefinition {
                model_name: M::model_name(),
                key: key.column(),
                parent: parent.column(),
                id: M::primary_key().column(),
                kind: kind.into(),
                sequence: sequence.definition.clone(),
            },
            model: PhantomData,
        }
    }
}

impl GeneratedKeyDefinition {
    pub(crate) fn validate_sync_registration(
        &self,
        registrations: &[crate::SyncRegistration],
    ) -> Result<(), MigrationError> {
        if let Some(model) = registrations
            .iter()
            .find(|model| model.schema.name == self.key.table)
        {
            if model.role == crate::ModelRole::Client {
                return Err(invalid("generated keys belong to the server schema"));
            }
            if !model
                .fields
                .iter()
                .any(|field| field.column == self.key.name && !field.writable)
            {
                return Err(invalid(
                    "a synchronized generated key must be a server-owned field",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn compile(
        &self,
        app: &str,
        state: &ProjectState,
    ) -> Result<Vec<crate::migration::DeclaredObject>, MigrationError> {
        let key = registered_column(state, self.key)?;
        let parent = registered_column(state, self.parent)?;
        let parent_id = registered_column(state, self.sequence.parent_id)?;
        if self.key.table != self.id.table
            || self.parent.table != self.id.table
            || !registered_column(state, self.id)?.primary_key
        {
            return Err(invalid(
                "key, primary key and parent reference must belong to the same model",
            ));
        }
        if key.column_type != ColumnType::Text
            || key.nullable
            || !key.unique
            || key.default.as_deref() != Some("''")
        {
            return Err(invalid(
                "generated key requires a unique, required text column with an empty string default",
            ));
        }
        if parent.nullable
            || parent.column_type != parent_id.column_type
            || parent.references.as_ref().and_then(|fk| fk.target_parts())
                != Some((self.sequence.parent_id.table, self.sequence.parent_id.name))
        {
            return Err(invalid(
                "generated key requires a non-null foreign key to the sequence's parent model",
            ));
        }
        if self.kind.is_empty()
            || !self
                .kind
                .bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        {
            return Err(invalid(
                "record kind must contain only uppercase ASCII letters or digits",
            ));
        }

        let table = quote(self.id.table);
        let id = quote(self.id.name);
        let key = quote(self.key.name);
        let parent = quote(self.parent.name);
        let parent_table = quote(self.sequence.parent_id.table);
        let parent_id = quote(self.sequence.parent_id.name);
        let prefix = quote(self.sequence.parent_key.name);
        let sequence = quote(&self.sequence.name);
        let kind = &self.kind;
        let label = kind.to_ascii_lowercase();
        let generated = format!(
            "(SELECT parent.{prefix} || '-{kind}-' || printf('%04d', seq.next_value) \
             FROM {parent_table} AS parent JOIN {sequence} AS seq ON seq.project_id = parent.{parent_id} AND seq.kind = '{kind}' \
             WHERE parent.{parent_id} = NEW.{parent})"
        );
        let key_requirements = BTreeSet::from([
            Req::field(self.key),
            Req::Default(self.key.table.into(), self.key.name.into()),
        ]);
        let mut assign = key_requirements.clone();
        assign.extend([
            Req::Relation(self.sequence.name.clone()),
            Req::field(self.id),
            Req::field(self.parent),
            Req::field(self.sequence.parent_id),
            Req::field(self.sequence.parent_key),
            Req::Key(
                self.sequence.parent_key.table.into(),
                self.sequence.parent_key.name.into(),
            ),
            Req::Key(self.key.table.into(), self.key.name.into()),
            Req::WriteConstraint(self.key.table.into(), self.key.name.into()),
        ]);
        let trigger = |suffix: &str, requirements: BTreeSet<Req>, statement: String| {
            let name = format!("{}_{suffix}", self.id.table);
            (
                SchemaObjectState {
                    app: app.into(),
                    kind: SchemaObjectKind::Trigger,
                    sql: format!("CREATE TRIGGER {} {statement}", quote(&name)),
                    name,
                },
                requirements,
            )
        };
        Ok(vec![
            trigger(
                "assign_key",
                assign,
                format!(
                    "AFTER INSERT ON {table} WHEN NEW.{key} = '' BEGIN \
                 INSERT INTO {sequence} (project_id, kind, next_value) VALUES (NEW.{parent}, '{kind}', 1) \
                 ON CONFLICT(project_id, kind) DO UPDATE SET next_value = next_value + 1; \
                 SELECT RAISE(ABORT, '{label} key already exists') \
                 WHERE EXISTS (SELECT 1 FROM {table} WHERE {key} = {generated}); \
                 UPDATE {table} SET {key} = {generated} WHERE {id} = NEW.{id}; END"
                ),
            ),
            trigger(
                "protect_key",
                BTreeSet::from([Req::field(self.key)]),
                format!(
                    "BEFORE UPDATE OF {key} ON {table} WHEN OLD.{key} <> '' AND OLD.{key} <> NEW.{key} \
                 BEGIN SELECT RAISE(ABORT, '{label} key cannot be changed'); END"
                ),
            ),
            trigger(
                "reject_explicit_key",
                key_requirements,
                format!(
                    "BEFORE INSERT ON {table} WHEN NEW.{key} <> '' \
                 BEGIN SELECT RAISE(ABORT, '{label} key is server-generated'); END"
                ),
            ),
        ])
    }
}

fn invalid(message: &str) -> MigrationError {
    MigrationError::InvalidState(format!("SQLite generated key: {message}"))
}
