use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::{ColumnSchema, ColumnType, ForeignKey, SchemaSet, TableSchema};

#[cfg(feature = "sqlite")]
pub mod sqlite_executor;

/// An owned foreign-key definition in a migration state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKeyState {
    pub target: String,
    pub on_delete: Option<String>,
}

/// An owned column definition in a migration state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnState {
    pub name: String,
    pub column_type: ColumnType,
    pub nullable: bool,
    pub primary_key: bool,
    pub unique: bool,
    pub default: Option<String>,
    pub check: Option<String>,
    pub choices: Option<Vec<String>>,
    pub references: Option<ForeignKeyState>,
    pub auto_now_add: bool,
    pub auto_now: bool,
}

/// An owned table definition in a migration state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableState {
    pub name: String,
    pub columns: Vec<ColumnState>,
    pub unique_constraints: Vec<Vec<String>>,
    pub indexes: Vec<Vec<String>>,
}

/// Stable identity of a model independently from its SQL table name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelKey {
    pub app: String,
    pub name: String,
}

/// A model and its current table definition in a historical project state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelState {
    pub key: ModelKey,
    pub table: TableState,
}

/// A deterministic difference between historical and current model state.
/// Field renames are intentionally represented as remove/add until the model
/// metadata gains a stable field identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateChange {
    CreateModel(ModelState),
    DeleteModel(ModelState),
    RenameTable {
        key: ModelKey,
        old_name: String,
        new_name: String,
    },
    AddColumn {
        key: ModelKey,
        table: String,
        column: ColumnState,
    },
    RemoveColumn {
        key: ModelKey,
        table: String,
        column: ColumnState,
    },
    AlterColumn {
        key: ModelKey,
        table: String,
        old_column: ColumnState,
        new_column: ColumnState,
    },
    AddIndex {
        key: ModelKey,
        table: String,
        columns: Vec<String>,
    },
    RemoveIndex {
        key: ModelKey,
        table: String,
        columns: Vec<String>,
    },
    AddUniqueConstraint {
        key: ModelKey,
        table: String,
        columns: Vec<String>,
    },
    RemoveUniqueConstraint {
        key: ModelKey,
        table: String,
        columns: Vec<String>,
    },
}

impl StateChange {
    /// Returns the application that owns the model being changed.
    pub fn app(&self) -> &str {
        match self {
            Self::CreateModel(model) | Self::DeleteModel(model) => &model.key.app,
            Self::RenameTable { key, .. }
            | Self::AddColumn { key, .. }
            | Self::RemoveColumn { key, .. }
            | Self::AlterColumn { key, .. }
            | Self::AddIndex { key, .. }
            | Self::RemoveIndex { key, .. }
            | Self::AddUniqueConstraint { key, .. }
            | Self::RemoveUniqueConstraint { key, .. } => &key.app,
        }
    }

    /// Converts a state difference into the forward-only operation that
    /// reproduces it. `ModelKey` is used while calculating the difference;
    /// the resulting operation carries the database state it needs to apply.
    pub fn into_operation(self) -> MigrationOperation {
        match self {
            Self::CreateModel(model) => MigrationOperation::CreateModel { model },
            Self::DeleteModel(model) => MigrationOperation::DropTable {
                name: model.table.name,
            },
            Self::RenameTable {
                old_name, new_name, ..
            } => MigrationOperation::RenameTable { old_name, new_name },
            Self::AddColumn { table, column, .. } => {
                MigrationOperation::AddColumn { table, column }
            }
            Self::RemoveColumn { table, column, .. } => MigrationOperation::RemoveColumn {
                table,
                column: column.name,
            },
            Self::AlterColumn {
                table,
                old_column,
                new_column,
                ..
            } => MigrationOperation::AlterColumn {
                table,
                old_column,
                new_column,
            },
            Self::AddIndex { table, columns, .. } => {
                MigrationOperation::AddIndex { table, columns }
            }
            Self::RemoveIndex { table, columns, .. } => {
                MigrationOperation::RemoveIndex { table, columns }
            }
            Self::AddUniqueConstraint { table, columns, .. } => {
                MigrationOperation::AddUniqueConstraint { table, columns }
            }
            Self::RemoveUniqueConstraint { table, columns, .. } => {
                MigrationOperation::RemoveUniqueConstraint { table, columns }
            }
        }
    }
}

/// Converts a deterministic schema difference into forward migration
/// operations, preserving the order chosen by [`ProjectState::diff`].
pub fn operations_from_changes(
    changes: impl IntoIterator<Item = StateChange>,
) -> Vec<MigrationOperation> {
    changes
        .into_iter()
        .map(StateChange::into_operation)
        .collect()
}

/// The fully owned schema produced by replaying migrations.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProjectState {
    tables: Vec<TableState>,
    model_keys: BTreeMap<String, ModelKey>,
}

impl ProjectState {
    /// Builds migration state from model-generated schema metadata.
    pub fn from_schema(schema: &SchemaSet) -> Self {
        Self {
            tables: schema.tables().iter().map(TableState::from).collect(),
            model_keys: BTreeMap::new(),
        }
    }

    /// Builds a historical state for models owned by one application. Model
    /// identity remains stable when a subsequent migration renames its table.
    pub fn from_app_schema(app: impl Into<String>, schema: &SchemaSet) -> Self {
        let app = app.into();
        let tables: Vec<_> = schema.tables().iter().map(TableState::from).collect();
        let model_keys = tables
            .iter()
            .zip(schema.model_names())
            .map(|(table, model_name)| {
                (
                    table.name.clone(),
                    ModelKey {
                        app: app.clone(),
                        name: (*model_name).into(),
                    },
                )
            })
            .collect();
        Self { tables, model_keys }
    }

    /// Combines schemas while retaining each application's model identity.
    pub fn from_app_schemas<'app, 'schema>(
        schemas: impl IntoIterator<Item = (&'app str, &'schema SchemaSet)>,
    ) -> Result<Self, MigrationError> {
        let mut state = Self::default();
        for (app, schema) in schemas {
            let app_state = Self::from_app_schema(app, schema);
            let mut keys: BTreeSet<_> = state.model_keys.values().cloned().collect();
            for key in app_state.model_keys.values() {
                if !keys.insert(key.clone()) {
                    return Err(MigrationError::InvalidState(format!(
                        "duplicate model {key:?}"
                    )));
                }
            }
            for table in app_state.tables {
                if state
                    .tables
                    .iter()
                    .any(|current| current.name == table.name)
                {
                    return Err(MigrationError::DuplicateTable(table.name));
                }
                state.tables.push(table);
            }
            state.model_keys.extend(app_state.model_keys);
        }
        Ok(state)
    }

    /// Returns tables in their current schema order.
    pub fn tables(&self) -> &[TableState] {
        &self.tables
    }

    /// Checks structural invariants without consulting a live database.
    pub fn validate(&self) -> Result<(), MigrationError> {
        let mut names = BTreeSet::new();
        for table in &self.tables {
            if !names.insert(&table.name) {
                return Err(MigrationError::DuplicateTable(table.name.clone()));
            }
            let mut columns = BTreeSet::new();
            for column in &table.columns {
                if !columns.insert(&column.name) {
                    return Err(MigrationError::DuplicateColumn {
                        table: table.name.clone(),
                        column: column.name.clone(),
                    });
                }
                if let Some(reference) = &column.references {
                    if parse_reference(&reference.target).is_none() {
                        return Err(MigrationError::InvalidState(format!(
                            "invalid reference {}",
                            reference.target
                        )));
                    }
                }
            }
            for constraint in table.indexes.iter().chain(&table.unique_constraints) {
                validate_constraint_columns(table, constraint)?;
            }
        }
        Ok(())
    }

    /// Returns models in table order, omitting legacy tables with no known
    /// application/model identity.
    pub fn models(&self) -> Vec<ModelState> {
        self.tables
            .iter()
            .filter_map(|table| {
                self.model_keys
                    .get(&table.name)
                    .cloned()
                    .map(|key| ModelState {
                        key,
                        table: table.clone(),
                    })
            })
            .collect()
    }

    /// Compares historical state with current model metadata without touching
    /// the database. Both states must have complete ModelKey coverage.
    pub fn diff(&self, desired: &Self) -> Result<Vec<StateChange>, MigrationError> {
        self.validate_model_coverage()?;
        desired.validate_model_coverage()?;
        let current: BTreeMap<_, _> = self
            .models()
            .into_iter()
            .map(|model| (model.key.clone(), model))
            .collect();
        let wanted: BTreeMap<_, _> = desired
            .models()
            .into_iter()
            .map(|model| (model.key.clone(), model))
            .collect();
        let mut changes = Vec::new();
        for (key, model) in &current {
            if !wanted.contains_key(key) {
                changes.push(StateChange::DeleteModel(model.clone()));
            }
        }
        for (key, model) in &wanted {
            let Some(old) = current.get(key) else {
                changes.push(StateChange::CreateModel(model.clone()));
                continue;
            };
            let old_table = &old.table;
            let new_table = &model.table;
            if old_table.name != new_table.name {
                changes.push(StateChange::RenameTable {
                    key: key.clone(),
                    old_name: old_table.name.clone(),
                    new_name: new_table.name.clone(),
                });
            }
            let old_columns: BTreeMap<_, _> = old_table
                .columns
                .iter()
                .map(|column| (column.name.clone(), column))
                .collect();
            let new_columns: BTreeMap<_, _> = new_table
                .columns
                .iter()
                .map(|column| (column.name.clone(), column))
                .collect();
            for (name, column) in &old_columns {
                if !new_columns.contains_key(name) {
                    changes.push(StateChange::RemoveColumn {
                        key: key.clone(),
                        table: new_table.name.clone(),
                        column: (*column).clone(),
                    });
                }
            }
            for (name, column) in &new_columns {
                match old_columns.get(name) {
                    None => changes.push(StateChange::AddColumn {
                        key: key.clone(),
                        table: new_table.name.clone(),
                        column: (*column).clone(),
                    }),
                    Some(old_column) if *old_column != *column => {
                        changes.push(StateChange::AlterColumn {
                            key: key.clone(),
                            table: new_table.name.clone(),
                            old_column: (*old_column).clone(),
                            new_column: (*column).clone(),
                        })
                    }
                    _ => {}
                }
            }
            diff_constraints(
                &mut changes,
                key,
                &new_table.name,
                &old_table.indexes,
                &new_table.indexes,
                true,
            );
            diff_constraints(
                &mut changes,
                key,
                &new_table.name,
                &old_table.unique_constraints,
                &new_table.unique_constraints,
                false,
            );
        }
        Ok(changes)
    }

    fn validate_model_coverage(&self) -> Result<(), MigrationError> {
        if self.model_keys.len() != self.tables.len() {
            return Err(MigrationError::InvalidState(
                "migration state is missing ModelKey metadata".into(),
            ));
        }
        self.validate()
    }
}

fn diff_constraints(
    changes: &mut Vec<StateChange>,
    key: &ModelKey,
    table: &str,
    old: &[Vec<String>],
    new: &[Vec<String>],
    index: bool,
) {
    for columns in old {
        if !new.contains(columns) {
            changes.push(if index {
                StateChange::RemoveIndex {
                    key: key.clone(),
                    table: table.into(),
                    columns: columns.clone(),
                }
            } else {
                StateChange::RemoveUniqueConstraint {
                    key: key.clone(),
                    table: table.into(),
                    columns: columns.clone(),
                }
            });
        }
    }
    for columns in new {
        if !old.contains(columns) {
            changes.push(if index {
                StateChange::AddIndex {
                    key: key.clone(),
                    table: table.into(),
                    columns: columns.clone(),
                }
            } else {
                StateChange::AddUniqueConstraint {
                    key: key.clone(),
                    table: table.into(),
                    columns: columns.clone(),
                }
            });
        }
    }
}

impl From<&SchemaSet> for ProjectState {
    fn from(schema: &SchemaSet) -> Self {
        Self::from_schema(schema)
    }
}

impl From<&ColumnSchema> for ColumnState {
    fn from(column: &ColumnSchema) -> Self {
        Self {
            name: column.name.into(),
            column_type: column.column_type,
            nullable: column.nullable,
            primary_key: column.primary_key,
            unique: column.unique,
            default: column.default.map(Into::into),
            check: column.check.map(Into::into),
            choices: column
                .choices
                .as_ref()
                .map(|choices| choices.iter().map(|choice| (*choice).into()).collect()),
            references: column.references.as_ref().map(ForeignKeyState::from),
            auto_now_add: column.auto_now_add,
            auto_now: column.auto_now,
        }
    }
}

impl From<&ForeignKey> for ForeignKeyState {
    fn from(foreign_key: &ForeignKey) -> Self {
        Self {
            target: foreign_key.target().into(),
            on_delete: foreign_key.on_delete().map(Into::into),
        }
    }
}

impl From<&TableSchema> for TableState {
    fn from(table: &TableSchema) -> Self {
        Self {
            name: table.name.into(),
            columns: table.columns.iter().map(ColumnState::from).collect(),
            unique_constraints: table
                .unique_constraints
                .iter()
                .map(|columns| columns.iter().map(|column| (*column).into()).collect())
                .collect(),
            indexes: table
                .indexes
                .iter()
                .map(|columns| columns.iter().map(|column| (*column).into()).collect())
                .collect(),
        }
    }
}

/// A stable migration name, scoped to an application.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MigrationId {
    pub app: String,
    pub name: String,
}

/// A declarative schema or SQL migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    pub id: MigrationId,
    pub dependencies: Vec<MigrationId>,
    pub operations: Vec<MigrationOperation>,
    /// A legacy user-supplied checksum or a `v1:` checksum calculated by
    /// [`Migration::new`]. New migrations should use the constructor.
    pub checksum: String,
}

impl Migration {
    /// Creates a migration with a checksum derived from its complete,
    /// declarative definition.
    pub fn new(
        id: MigrationId,
        dependencies: Vec<MigrationId>,
        operations: Vec<MigrationOperation>,
    ) -> Self {
        let mut migration = Self {
            id,
            dependencies,
            operations,
            checksum: String::new(),
        };
        migration.checksum = migration.calculated_checksum();
        migration
    }

    /// Returns the versioned checksum for this migration's id, dependencies
    /// and operations. The encoding preserves operation order and normalizes
    /// dependency order.
    pub fn calculated_checksum(&self) -> String {
        let mut bytes = Vec::new();
        encode_string(&mut bytes, "che-orm-migration");
        encode_string(&mut bytes, "v1");
        encode_migration_id(&mut bytes, &self.id);
        let mut dependencies = self.dependencies.clone();
        dependencies.sort();
        encode_usize(&mut bytes, dependencies.len());
        for dependency in &dependencies {
            encode_migration_id(&mut bytes, dependency);
        }
        encode_usize(&mut bytes, self.operations.len());
        for operation in &self.operations {
            encode_operation(&mut bytes, operation);
        }
        format!("v1:{:016x}", fnv1a64(&bytes))
    }

    fn validates_checksum(&self) -> bool {
        !self.checksum.starts_with("v1:") || self.checksum == self.calculated_checksum()
    }
}

/// Renders a self-contained Rust migration definition. The output deliberately
/// calls [`Migration::new`], so its checksum always describes the source that
/// will be compiled and registered.
pub fn render_migration_rust(migration: &Migration) -> String {
    let mut source = String::from(
        "use che_orm::{ColumnState, ColumnType, ForeignKeyState, Migration, MigrationId, MigrationOperation, ModelKey, ModelState, TableState};\n\n",
    );
    source.push_str("pub fn migration() -> Migration {\n    Migration::new(\n");
    source.push_str(&format!(
        "        {},\n",
        render_migration_id(&migration.id)
    ));
    source.push_str("        vec![");
    for (index, dependency) in migration.dependencies.iter().enumerate() {
        if index > 0 {
            source.push_str(", ");
        }
        source.push_str(&render_migration_id(dependency));
    }
    source.push_str("],\n        vec![\n");
    for operation in &migration.operations {
        source.push_str("            ");
        source.push_str(&render_operation(operation, 12));
        source.push_str(",\n");
    }
    source.push_str("        ],\n    )\n}\n");
    source
}

fn render_string(value: &str) -> String {
    format!("{value:?}.into()")
}

fn render_migration_id(id: &MigrationId) -> String {
    format!(
        "MigrationId {{ app: {}, name: {} }}",
        render_string(&id.app),
        render_string(&id.name)
    )
}

fn render_strings(values: &[String]) -> String {
    format!(
        "vec![{}]",
        values
            .iter()
            .map(|value| render_string(value))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn render_column_type(column_type: ColumnType) -> &'static str {
    match column_type {
        ColumnType::Integer => "ColumnType::Integer",
        ColumnType::Text => "ColumnType::Text",
        ColumnType::Binary => "ColumnType::Binary",
        ColumnType::Boolean => "ColumnType::Boolean",
        ColumnType::DateTime => "ColumnType::DateTime",
    }
}

fn render_option_string(value: &Option<String>) -> String {
    match value {
        Some(value) => format!("Some({})", render_string(value)),
        None => "None".into(),
    }
}

fn render_column(column: &ColumnState) -> String {
    let choices = match &column.choices {
        Some(choices) => format!("Some({})", render_strings(choices)),
        None => "None".into(),
    };
    let references = match &column.references {
        Some(reference) => format!(
            "Some(ForeignKeyState {{ target: {}, on_delete: {} }})",
            render_string(&reference.target),
            render_option_string(&reference.on_delete)
        ),
        None => "None".into(),
    };
    format!(
        "ColumnState {{ name: {}, column_type: {}, nullable: {}, primary_key: {}, unique: {}, default: {}, check: {}, choices: {}, references: {}, auto_now_add: {}, auto_now: {} }}",
        render_string(&column.name),
        render_column_type(column.column_type),
        column.nullable,
        column.primary_key,
        column.unique,
        render_option_string(&column.default),
        render_option_string(&column.check),
        choices,
        references,
        column.auto_now_add,
        column.auto_now,
    )
}

fn render_table(table: &TableState) -> String {
    let columns = table
        .columns
        .iter()
        .map(render_column)
        .collect::<Vec<_>>()
        .join(", ");
    let constraints = |constraints: &[Vec<String>]| {
        format!(
            "vec![{}]",
            constraints
                .iter()
                .map(|columns| render_strings(columns))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "TableState {{ name: {}, columns: vec![{}], unique_constraints: {}, indexes: {} }}",
        render_string(&table.name),
        columns,
        constraints(&table.unique_constraints),
        constraints(&table.indexes)
    )
}

fn render_model(model: &ModelState) -> String {
    format!(
        "ModelState {{ key: ModelKey {{ app: {}, name: {} }}, table: {} }}",
        render_string(&model.key.app),
        render_string(&model.key.name),
        render_table(&model.table)
    )
}

fn render_operation(operation: &MigrationOperation, _indent: usize) -> String {
    match operation {
        MigrationOperation::CreateModel { model } => format!(
            "MigrationOperation::CreateModel {{ model: {} }}",
            render_model(model)
        ),
        MigrationOperation::CreateTable { table } => format!(
            "MigrationOperation::CreateTable {{ table: {} }}",
            render_table(table)
        ),
        MigrationOperation::DropTable { name } => format!(
            "MigrationOperation::DropTable {{ name: {} }}",
            render_string(name)
        ),
        MigrationOperation::AddColumn { table, column } => format!(
            "MigrationOperation::AddColumn {{ table: {}, column: {} }}",
            render_string(table),
            render_column(column)
        ),
        MigrationOperation::RemoveColumn { table, column } => format!(
            "MigrationOperation::RemoveColumn {{ table: {}, column: {} }}",
            render_string(table),
            render_string(column)
        ),
        MigrationOperation::RenameTable { old_name, new_name } => format!(
            "MigrationOperation::RenameTable {{ old_name: {}, new_name: {} }}",
            render_string(old_name),
            render_string(new_name)
        ),
        MigrationOperation::RenameColumn {
            table,
            old_name,
            new_name,
        } => format!(
            "MigrationOperation::RenameColumn {{ table: {}, old_name: {}, new_name: {} }}",
            render_string(table),
            render_string(old_name),
            render_string(new_name)
        ),
        MigrationOperation::AlterColumn {
            table,
            old_column,
            new_column,
        } => format!(
            "MigrationOperation::AlterColumn {{ table: {}, old_column: {}, new_column: {} }}",
            render_string(table),
            render_column(old_column),
            render_column(new_column)
        ),
        MigrationOperation::AddIndex { table, columns } => format!(
            "MigrationOperation::AddIndex {{ table: {}, columns: {} }}",
            render_string(table),
            render_strings(columns)
        ),
        MigrationOperation::RemoveIndex { table, columns } => format!(
            "MigrationOperation::RemoveIndex {{ table: {}, columns: {} }}",
            render_string(table),
            render_strings(columns)
        ),
        MigrationOperation::AddUniqueConstraint { table, columns } => format!(
            "MigrationOperation::AddUniqueConstraint {{ table: {}, columns: {} }}",
            render_string(table),
            render_strings(columns)
        ),
        MigrationOperation::RemoveUniqueConstraint { table, columns } => format!(
            "MigrationOperation::RemoveUniqueConstraint {{ table: {}, columns: {} }}",
            render_string(table),
            render_strings(columns)
        ),
        MigrationOperation::RunSql {
            forward,
            reverse,
            state_operations,
        } => format!(
            "MigrationOperation::RunSql {{ forward: {}, reverse: {}, state_operations: vec![{}] }}",
            render_string(forward),
            render_option_string(reverse),
            state_operations
                .iter()
                .map(|operation| render_operation(operation, 0))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// A migration state transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationOperation {
    CreateModel {
        model: ModelState,
    },
    CreateTable {
        table: TableState,
    },
    DropTable {
        name: String,
    },
    AddColumn {
        table: String,
        column: ColumnState,
    },
    RemoveColumn {
        table: String,
        column: String,
    },
    RenameTable {
        old_name: String,
        new_name: String,
    },
    RenameColumn {
        table: String,
        old_name: String,
        new_name: String,
    },
    AlterColumn {
        table: String,
        old_column: ColumnState,
        new_column: ColumnState,
    },
    AddIndex {
        table: String,
        columns: Vec<String>,
    },
    RemoveIndex {
        table: String,
        columns: Vec<String>,
    },
    AddUniqueConstraint {
        table: String,
        columns: Vec<String>,
    },
    RemoveUniqueConstraint {
        table: String,
        columns: Vec<String>,
    },
    RunSql {
        forward: String,
        reverse: Option<String>,
        state_operations: Vec<MigrationOperation>,
    },
}

fn encode_usize(bytes: &mut Vec<u8>, value: usize) {
    bytes.extend_from_slice(&(value as u64).to_be_bytes());
}

fn encode_string(bytes: &mut Vec<u8>, value: &str) {
    encode_usize(bytes, value.len());
    bytes.extend_from_slice(value.as_bytes());
}

fn encode_bool(bytes: &mut Vec<u8>, value: bool) {
    bytes.push(u8::from(value));
}

fn encode_option_string(bytes: &mut Vec<u8>, value: &Option<String>) {
    match value {
        Some(value) => {
            bytes.push(1);
            encode_string(bytes, value);
        }
        None => bytes.push(0),
    }
}

fn encode_migration_id(bytes: &mut Vec<u8>, id: &MigrationId) {
    encode_string(bytes, &id.app);
    encode_string(bytes, &id.name);
}

fn encode_column(bytes: &mut Vec<u8>, column: &ColumnState) {
    encode_string(bytes, &column.name);
    bytes.push(match column.column_type {
        ColumnType::Integer => 0,
        ColumnType::Text => 1,
        ColumnType::Binary => 2,
        ColumnType::Boolean => 3,
        ColumnType::DateTime => 4,
    });
    encode_bool(bytes, column.nullable);
    encode_bool(bytes, column.primary_key);
    encode_bool(bytes, column.unique);
    encode_option_string(bytes, &column.default);
    encode_option_string(bytes, &column.check);
    match &column.choices {
        Some(choices) => {
            bytes.push(1);
            encode_usize(bytes, choices.len());
            for choice in choices {
                encode_string(bytes, choice);
            }
        }
        None => bytes.push(0),
    }
    match &column.references {
        Some(reference) => {
            bytes.push(1);
            encode_string(bytes, &reference.target);
            encode_option_string(bytes, &reference.on_delete);
        }
        None => bytes.push(0),
    }
    encode_bool(bytes, column.auto_now_add);
    encode_bool(bytes, column.auto_now);
}

fn encode_table(bytes: &mut Vec<u8>, table: &TableState) {
    encode_string(bytes, &table.name);
    encode_usize(bytes, table.columns.len());
    for column in &table.columns {
        encode_column(bytes, column);
    }
    for constraints in [&table.unique_constraints, &table.indexes] {
        encode_usize(bytes, constraints.len());
        for columns in constraints {
            encode_usize(bytes, columns.len());
            for column in columns {
                encode_string(bytes, column);
            }
        }
    }
}

fn encode_operation(bytes: &mut Vec<u8>, operation: &MigrationOperation) {
    match operation {
        MigrationOperation::CreateModel { model } => {
            bytes.push(12);
            encode_string(bytes, &model.key.app);
            encode_string(bytes, &model.key.name);
            encode_table(bytes, &model.table);
        }
        MigrationOperation::CreateTable { table } => {
            bytes.push(0);
            encode_table(bytes, table);
        }
        MigrationOperation::DropTable { name } => {
            bytes.push(1);
            encode_string(bytes, name);
        }
        MigrationOperation::AddColumn { table, column } => {
            bytes.push(2);
            encode_string(bytes, table);
            encode_column(bytes, column);
        }
        MigrationOperation::RemoveColumn { table, column } => {
            bytes.push(3);
            encode_string(bytes, table);
            encode_string(bytes, column);
        }
        MigrationOperation::RenameTable { old_name, new_name } => {
            bytes.push(4);
            encode_string(bytes, old_name);
            encode_string(bytes, new_name);
        }
        MigrationOperation::RenameColumn {
            table,
            old_name,
            new_name,
        } => {
            bytes.push(5);
            encode_string(bytes, table);
            encode_string(bytes, old_name);
            encode_string(bytes, new_name);
        }
        MigrationOperation::RunSql {
            forward,
            reverse,
            state_operations,
        } => {
            bytes.push(6);
            encode_string(bytes, forward);
            encode_option_string(bytes, reverse);
            encode_usize(bytes, state_operations.len());
            for operation in state_operations {
                encode_operation(bytes, operation);
            }
        }
        MigrationOperation::AlterColumn {
            table,
            old_column,
            new_column,
        } => {
            bytes.push(7);
            encode_string(bytes, table);
            encode_column(bytes, old_column);
            encode_column(bytes, new_column);
        }
        MigrationOperation::AddIndex { table, columns } => {
            bytes.push(8);
            encode_string(bytes, table);
            encode_string_list(bytes, columns);
        }
        MigrationOperation::RemoveIndex { table, columns } => {
            bytes.push(9);
            encode_string(bytes, table);
            encode_string_list(bytes, columns);
        }
        MigrationOperation::AddUniqueConstraint { table, columns } => {
            bytes.push(10);
            encode_string(bytes, table);
            encode_string_list(bytes, columns);
        }
        MigrationOperation::RemoveUniqueConstraint { table, columns } => {
            bytes.push(11);
            encode_string(bytes, table);
            encode_string_list(bytes, columns);
        }
    }
}

fn encode_string_list(bytes: &mut Vec<u8>, values: &[String]) {
    encode_usize(bytes, values.len());
    for value in values {
        encode_string(bytes, value);
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

impl MigrationOperation {
    /// Applies this operation to the in-memory schema state.
    pub fn state_forwards(&self, state: &mut ProjectState) -> Result<(), MigrationError> {
        match self {
            Self::CreateModel { model } => {
                if state.model_keys.values().any(|key| key == &model.key) {
                    return Err(MigrationError::InvalidState(format!(
                        "duplicate model {:?}",
                        model.key
                    )));
                }
                Self::CreateTable {
                    table: model.table.clone(),
                }
                .state_forwards(state)?;
                state
                    .model_keys
                    .insert(model.table.name.clone(), model.key.clone());
            }
            Self::CreateTable { table } => {
                if state
                    .tables
                    .iter()
                    .any(|current| current.name == table.name)
                {
                    return Err(MigrationError::DuplicateTable(table.name.clone()));
                }
                state.tables.push(table.clone());
            }
            Self::DropTable { name } => {
                if let Some((table, column)) = inbound_reference(state, name, None) {
                    return Err(MigrationError::DependentTable {
                        table: name.clone(),
                        dependent: format!("{table}.{column}"),
                    });
                }
                let index = table_index(state, name)?;
                state.tables.remove(index);
                state.model_keys.remove(name);
            }
            Self::AddColumn { table, column } => {
                let table = table_mut(state, table)?;
                if table
                    .columns
                    .iter()
                    .any(|current| current.name == column.name)
                {
                    return Err(MigrationError::DuplicateColumn {
                        table: table.name.clone(),
                        column: column.name.clone(),
                    });
                }
                table.columns.push(column.clone());
            }
            Self::RemoveColumn { table, column } => {
                if let Some((dependent_table, dependent_column)) =
                    inbound_reference(state, table, Some(column))
                {
                    return Err(MigrationError::DependentColumn {
                        table: table.clone(),
                        column: column.clone(),
                        dependent: format!("{dependent_table}.{dependent_column}"),
                    });
                }
                let table = table_mut(state, table)?;
                if table
                    .unique_constraints
                    .iter()
                    .chain(table.indexes.iter())
                    .any(|columns| columns.iter().any(|current| current == column))
                {
                    return Err(MigrationError::DependentColumn {
                        table: table.name.clone(),
                        column: column.clone(),
                        dependent: "an index or unique constraint".into(),
                    });
                }
                let index = column_index(table, column)?;
                table.columns.remove(index);
            }
            Self::RenameTable { old_name, new_name } => {
                if state.tables.iter().any(|table| table.name == *new_name) {
                    return Err(MigrationError::DuplicateTable(new_name.clone()));
                }
                table_mut(state, old_name)?.name = new_name.clone();
                if let Some(model_key) = state.model_keys.remove(old_name) {
                    state.model_keys.insert(new_name.clone(), model_key);
                }
                for table in &mut state.tables {
                    for column in &mut table.columns {
                        if let Some(reference) = &mut column.references {
                            reference.target =
                                rename_reference_table(&reference.target, old_name, new_name);
                        }
                    }
                }
            }
            Self::RenameColumn {
                table,
                old_name,
                new_name,
            } => {
                let existing = &state.tables[table_index(state, table)?];
                if existing.columns.iter().any(|column| column.check.is_some()) {
                    return Err(MigrationError::InvalidState(format!(
                        "cannot rename a column of {table} with opaque CHECK expressions; use explicit RunSql and matching state operations"
                    )));
                }
                let table_name = {
                    let table = table_mut(state, table)?;
                    if table.columns.iter().any(|column| column.name == *new_name) {
                        return Err(MigrationError::DuplicateColumn {
                            table: table.name.clone(),
                            column: new_name.clone(),
                        });
                    }
                    let index = column_index(table, old_name)?;
                    table.columns[index].name = new_name.clone();
                    rename_constraint_columns(&mut table.unique_constraints, old_name, new_name);
                    rename_constraint_columns(&mut table.indexes, old_name, new_name);
                    table.name.clone()
                };
                for dependent_table in &mut state.tables {
                    for column in &mut dependent_table.columns {
                        if let Some(reference) = &mut column.references {
                            reference.target = rename_reference_column(
                                &reference.target,
                                &table_name,
                                old_name,
                                new_name,
                            );
                        }
                    }
                }
            }
            Self::AlterColumn {
                table,
                old_column,
                new_column,
            } => {
                if old_column.name != new_column.name {
                    return Err(MigrationError::AlteredColumnRenamed {
                        table: table.clone(),
                        old_name: old_column.name.clone(),
                        new_name: new_column.name.clone(),
                    });
                }
                let table = table_mut(state, table)?;
                let index = column_index(table, &old_column.name)?;
                if table.columns[index] != *old_column {
                    return Err(MigrationError::UnexpectedColumnState {
                        table: table.name.clone(),
                        column: old_column.name.clone(),
                    });
                }
                table.columns[index] = new_column.clone();
            }
            Self::AddIndex { table, columns } => {
                let table = table_mut(state, table)?;
                validate_constraint_columns(table, columns)?;
                if table.indexes.iter().any(|index| index == columns) {
                    return Err(MigrationError::DuplicateIndex {
                        table: table.name.clone(),
                        columns: columns.clone(),
                    });
                }
                table.indexes.push(columns.clone());
            }
            Self::RemoveIndex { table, columns } => {
                let table = table_mut(state, table)?;
                remove_constraint(&mut table.indexes, columns, &table.name, true)?;
            }
            Self::AddUniqueConstraint { table, columns } => {
                let table = table_mut(state, table)?;
                validate_constraint_columns(table, columns)?;
                if table
                    .unique_constraints
                    .iter()
                    .any(|constraint| constraint == columns)
                {
                    return Err(MigrationError::DuplicateUniqueConstraint {
                        table: table.name.clone(),
                        columns: columns.clone(),
                    });
                }
                table.unique_constraints.push(columns.clone());
            }
            Self::RemoveUniqueConstraint { table, columns } => {
                let table = table_mut(state, table)?;
                remove_constraint(&mut table.unique_constraints, columns, &table.name, false)?;
            }
            Self::RunSql {
                state_operations, ..
            } => {
                for operation in state_operations {
                    operation.state_forwards(state)?;
                }
            }
        }
        Ok(())
    }
}

fn table_index(state: &ProjectState, name: &str) -> Result<usize, MigrationError> {
    state
        .tables
        .iter()
        .position(|table| table.name == name)
        .ok_or_else(|| MigrationError::MissingTable(name.into()))
}

fn table_mut<'a>(
    state: &'a mut ProjectState,
    name: &str,
) -> Result<&'a mut TableState, MigrationError> {
    let index = table_index(state, name)?;
    Ok(&mut state.tables[index])
}

fn column_index(table: &TableState, name: &str) -> Result<usize, MigrationError> {
    table
        .columns
        .iter()
        .position(|column| column.name == name)
        .ok_or_else(|| MigrationError::MissingColumn {
            table: table.name.clone(),
            column: name.into(),
        })
}

fn inbound_reference(
    state: &ProjectState,
    target_table: &str,
    target_column: Option<&str>,
) -> Option<(String, String)> {
    state.tables.iter().find_map(|table| {
        table.columns.iter().find_map(|column| {
            let reference = column.references.as_ref()?;
            let (table_name, column_name) = parse_reference(&reference.target)?;
            (table_name == target_table
                && target_column.is_none_or(|target_column| column_name == target_column))
            .then(|| (table.name.clone(), column.name.clone()))
        })
    })
}

fn parse_reference(reference: &str) -> Option<(&str, &str)> {
    let (table, column) = reference.split_once('(')?;
    column.strip_suffix(')').map(|column| (table, column))
}

fn rename_reference_table(reference: &str, old_name: &str, new_name: &str) -> String {
    let Some((table, column)) = parse_reference(reference) else {
        return reference.into();
    };
    if table == old_name {
        format!("{new_name}({column})")
    } else {
        reference.into()
    }
}

fn rename_reference_column(
    reference: &str,
    table_name: &str,
    old_name: &str,
    new_name: &str,
) -> String {
    let Some((table, column)) = parse_reference(reference) else {
        return reference.into();
    };
    if table == table_name && column == old_name {
        format!("{table_name}({new_name})")
    } else {
        reference.into()
    }
}

fn rename_constraint_columns(constraints: &mut [Vec<String>], old_name: &str, new_name: &str) {
    for columns in constraints {
        for column in columns {
            if column == old_name {
                *column = new_name.into();
            }
        }
    }
}

fn validate_constraint_columns(
    table: &TableState,
    columns: &[String],
) -> Result<(), MigrationError> {
    if columns.is_empty() {
        return Err(MigrationError::EmptyConstraint(table.name.clone()));
    }
    for column in columns {
        column_index(table, column)?;
    }
    Ok(())
}

fn remove_constraint(
    constraints: &mut Vec<Vec<String>>,
    columns: &[String],
    table: &str,
    index: bool,
) -> Result<(), MigrationError> {
    let Some(position) = constraints
        .iter()
        .position(|constraint| constraint == columns)
    else {
        return Err(if index {
            MigrationError::MissingIndex {
                table: table.into(),
                columns: columns.to_vec(),
            }
        } else {
            MigrationError::MissingUniqueConstraint {
                table: table.into(),
                columns: columns.to_vec(),
            }
        });
    };
    constraints.remove(position);
    Ok(())
}

/// Errors while validating or replaying migrations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationError {
    InvalidState(String),
    DuplicateMigration(MigrationId),
    InvalidChecksum(MigrationId),
    MissingDependency {
        migration: MigrationId,
        dependency: MigrationId,
    },
    CyclicDependency(Vec<MigrationId>),
    UnknownTarget(MigrationId),
    UnknownApplied(MigrationId),
    InconsistentAppliedHistory {
        migration: MigrationId,
        dependency: MigrationId,
    },
    DuplicateTable(String),
    MissingTable(String),
    DuplicateColumn {
        table: String,
        column: String,
    },
    MissingColumn {
        table: String,
        column: String,
    },
    AlteredColumnRenamed {
        table: String,
        old_name: String,
        new_name: String,
    },
    UnexpectedColumnState {
        table: String,
        column: String,
    },
    EmptyConstraint(String),
    DuplicateIndex {
        table: String,
        columns: Vec<String>,
    },
    MissingIndex {
        table: String,
        columns: Vec<String>,
    },
    DuplicateUniqueConstraint {
        table: String,
        columns: Vec<String>,
    },
    MissingUniqueConstraint {
        table: String,
        columns: Vec<String>,
    },
    DependentTable {
        table: String,
        dependent: String,
    },
    DependentColumn {
        table: String,
        column: String,
        dependent: String,
    },
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidState(message) => formatter.write_str(message),
            Self::DuplicateMigration(id) => {
                write!(formatter, "duplicate migration {}:{}", id.app, id.name)
            }
            Self::InvalidChecksum(id) => {
                write!(
                    formatter,
                    "migration {}:{} has an invalid checksum",
                    id.app, id.name
                )
            }
            Self::MissingDependency {
                migration,
                dependency,
            } => write!(
                formatter,
                "migration {}:{} depends on missing {}:{}",
                migration.app, migration.name, dependency.app, dependency.name
            ),
            Self::CyclicDependency(ids) => {
                write!(formatter, "cyclic migration dependencies: {ids:?}")
            }
            Self::UnknownTarget(id) => {
                write!(formatter, "unknown migration target {}:{}", id.app, id.name)
            }
            Self::UnknownApplied(id) => {
                write!(
                    formatter,
                    "unknown applied migration {}:{}",
                    id.app, id.name
                )
            }
            Self::InconsistentAppliedHistory {
                migration,
                dependency,
            } => write!(
                formatter,
                "applied migration {}:{} is missing dependency {}:{}",
                migration.app, migration.name, dependency.app, dependency.name
            ),
            Self::DuplicateTable(name) => write!(formatter, "duplicate table {name}"),
            Self::MissingTable(name) => write!(formatter, "missing table {name}"),
            Self::DuplicateColumn { table, column } => {
                write!(formatter, "duplicate column {table}.{column}")
            }
            Self::MissingColumn { table, column } => {
                write!(formatter, "missing column {table}.{column}")
            }
            Self::AlteredColumnRenamed {
                table,
                old_name,
                new_name,
            } => write!(
                formatter,
                "AlterColumn for {table} cannot rename {old_name} to {new_name}; use RenameColumn"
            ),
            Self::UnexpectedColumnState { table, column } => {
                write!(
                    formatter,
                    "unexpected historical state for column {table}.{column}"
                )
            }
            Self::EmptyConstraint(table) => write!(formatter, "empty constraint on table {table}"),
            Self::DuplicateIndex { table, columns } => {
                write!(
                    formatter,
                    "duplicate index on {table}({})",
                    columns.join(", ")
                )
            }
            Self::MissingIndex { table, columns } => {
                write!(
                    formatter,
                    "missing index on {table}({})",
                    columns.join(", ")
                )
            }
            Self::DuplicateUniqueConstraint { table, columns } => write!(
                formatter,
                "duplicate unique constraint on {table}({})",
                columns.join(", ")
            ),
            Self::MissingUniqueConstraint { table, columns } => write!(
                formatter,
                "missing unique constraint on {table}({})",
                columns.join(", ")
            ),
            Self::DependentTable { table, dependent } => {
                write!(
                    formatter,
                    "cannot remove table {table}; it is referenced by {dependent}"
                )
            }
            Self::DependentColumn {
                table,
                column,
                dependent,
            } => write!(
                formatter,
                "cannot remove column {table}.{column}; it is used by {dependent}"
            ),
        }
    }
}

impl std::error::Error for MigrationError {}

/// A validated migration dependency graph with deterministic replay order.
#[derive(Debug, Clone)]
pub struct MigrationGraph {
    migrations: BTreeMap<MigrationId, Migration>,
    order: Vec<MigrationId>,
}

/// One migration selected by the planner. SQL rendering and execution are
/// deliberately separate from dependency planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPlanStep {
    pub id: MigrationId,
}

impl MigrationGraph {
    /// Validates migrations and computes their lexically deterministic topological order.
    pub fn new(migrations: Vec<Migration>) -> Result<Self, MigrationError> {
        let mut by_id = BTreeMap::new();
        for migration in migrations {
            let id = migration.id.clone();
            if by_id.insert(id.clone(), migration).is_some() {
                return Err(MigrationError::DuplicateMigration(id));
            }
        }
        for migration in by_id.values() {
            if !migration.validates_checksum() {
                return Err(MigrationError::InvalidChecksum(migration.id.clone()));
            }
            for dependency in &migration.dependencies {
                if !by_id.contains_key(dependency) {
                    return Err(MigrationError::MissingDependency {
                        migration: migration.id.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
        }

        let mut remaining: BTreeMap<_, BTreeSet<_>> = by_id
            .iter()
            .map(|(id, migration)| (id.clone(), migration.dependencies.iter().cloned().collect()))
            .collect();
        let mut order = Vec::with_capacity(by_id.len());
        while !remaining.is_empty() {
            let ready: Vec<_> = remaining
                .iter()
                .filter(|(_, dependencies)| dependencies.is_empty())
                .map(|(id, _)| id.clone())
                .collect();
            if ready.is_empty() {
                return Err(MigrationError::CyclicDependency(
                    remaining.into_keys().collect(),
                ));
            }
            for id in ready {
                remaining.remove(&id);
                for dependencies in remaining.values_mut() {
                    dependencies.remove(&id);
                }
                order.push(id);
            }
        }
        Ok(Self {
            migrations: by_id,
            order,
        })
    }

    /// Returns migrations in deterministic dependency order.
    pub fn ordered(&self) -> impl Iterator<Item = &Migration> {
        self.order.iter().map(|id| &self.migrations[id])
    }

    /// Returns the terminal migrations for one application. Multiple heads
    /// indicate a branch that needs a merge migration before release.
    pub fn app_heads(&self, app: &str) -> Vec<&Migration> {
        let depended_on: BTreeSet<_> = self
            .migrations
            .values()
            .filter(|migration| migration.id.app == app)
            .flat_map(|migration| migration.dependencies.iter())
            .filter(|dependency| dependency.app == app)
            .collect();
        self.ordered()
            .filter(|migration| migration.id.app == app && !depended_on.contains(&migration.id))
            .collect()
    }

    /// Builds and validates a forward-only plan. Independent applied migrations
    /// are retained. A target already passed in its app is rejected.
    pub fn plan_to(
        &self,
        applied: &BTreeSet<MigrationId>,
        target: Option<&MigrationId>,
    ) -> Result<Vec<MigrationPlanStep>, MigrationError> {
        self.plan_forward(applied, target)
    }

    /// Returns pending steps only, after checking graph and historical state.
    pub fn plan_forward(
        &self,
        applied: &BTreeSet<MigrationId>,
        target: Option<&MigrationId>,
    ) -> Result<Vec<MigrationPlanStep>, MigrationError> {
        self.validate_applied(applied)?;
        self.validate_heads()?;
        if let Some(target) = target {
            if target.name == "zero" {
                return Err(MigrationError::InvalidState(
                    "migration rollback is not supported".into(),
                ));
            }
            for id in applied
                .iter()
                .filter(|id| id.app == target.app && *id != target)
            {
                if self.target_closure(id)?.contains(target) {
                    return Err(MigrationError::InvalidState(
                        "migration rollback is not supported".into(),
                    ));
                }
            }
        }
        let desired = match target {
            Some(target) => self.target_closure(target)?,
            None => self.migrations.keys().cloned().collect(),
        };

        let mut plan = Vec::new();
        let mut state = self.replay_selected(ProjectState::default(), applied)?;
        for migration in self.ordered() {
            if desired.contains(&migration.id) && !applied.contains(&migration.id) {
                for operation in &migration.operations {
                    operation.state_forwards(&mut state)?;
                    state.validate()?;
                }
                plan.push(MigrationPlanStep {
                    id: migration.id.clone(),
                });
            }
        }
        Ok(plan)
    }

    pub fn validate_heads(&self) -> Result<(), MigrationError> {
        let apps: BTreeSet<_> = self.migrations.keys().map(|id| id.app.as_str()).collect();
        for app in apps {
            if self.app_heads(app).len() > 1 {
                return Err(MigrationError::InvalidState(format!(
                    "conflicting migration heads in {app}; merge required"
                )));
            }
        }
        Ok(())
    }

    fn target_closure(
        &self,
        target: &MigrationId,
    ) -> Result<BTreeSet<MigrationId>, MigrationError> {
        if !self.migrations.contains_key(target) {
            return Err(MigrationError::UnknownTarget(target.clone()));
        }
        let mut desired = BTreeSet::new();
        let mut pending = vec![target.clone()];
        while let Some(id) = pending.pop() {
            if !desired.insert(id.clone()) {
                continue;
            }
            pending.extend(self.migrations[&id].dependencies.iter().cloned());
        }
        Ok(desired)
    }

    fn validate_applied(&self, applied: &BTreeSet<MigrationId>) -> Result<(), MigrationError> {
        for id in applied {
            let migration = self
                .migrations
                .get(id)
                .ok_or_else(|| MigrationError::UnknownApplied(id.clone()))?;
            for dependency in &migration.dependencies {
                if !applied.contains(dependency) {
                    return Err(MigrationError::InconsistentAppliedHistory {
                        migration: id.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Replays every migration from an initial schema state.
    pub fn replay(&self, mut state: ProjectState) -> Result<ProjectState, MigrationError> {
        for migration in self.ordered() {
            for operation in &migration.operations {
                operation.state_forwards(&mut state)?;
                state.validate()?;
            }
        }
        Ok(state)
    }

    /// Replays a closed subset of migrations in graph order. Executors use
    /// this to reconstruct the state immediately before pending migrations.
    pub fn replay_selected(
        &self,
        mut state: ProjectState,
        selected: &BTreeSet<MigrationId>,
    ) -> Result<ProjectState, MigrationError> {
        self.validate_applied(selected)?;
        for migration in self.ordered() {
            if selected.contains(&migration.id) {
                for operation in &migration.operations {
                    operation.state_forwards(&mut state)?;
                    state.validate()?;
                }
            }
        }
        Ok(state)
    }
}

/// SQL and resulting historical state for one SQLite schema operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteOperationPlan {
    pub statements: Vec<String>,
    pub resulting_state: ProjectState,
}

/// Renders declarative migration operations for SQLite. Operations that alter
/// a table's shape use SQLite's reliable create-copy-drop-rename workflow.
#[derive(Debug, Default, Clone, Copy)]
pub struct SqliteSchemaEditor;

impl SqliteSchemaEditor {
    /// Returns the affected table for operations implemented through a table
    /// rebuild. Callers can inspect database-owned triggers before execution.
    pub fn rebuild_table_name(operation: &MigrationOperation) -> Option<&str> {
        match operation {
            MigrationOperation::AddIndex { table, .. }
            | MigrationOperation::RemoveIndex { table, .. }
            | MigrationOperation::AddUniqueConstraint { table, .. }
            | MigrationOperation::RemoveUniqueConstraint { table, .. }
            | MigrationOperation::AddColumn { table, .. }
            | MigrationOperation::RemoveColumn { table, .. }
            | MigrationOperation::AlterColumn { table, .. } => Some(table),
            _ => None,
        }
    }

    pub fn plan(
        operation: &MigrationOperation,
        before: &ProjectState,
    ) -> Result<SqliteOperationPlan, MigrationError> {
        let mut resulting_state = before.clone();
        operation.state_forwards(&mut resulting_state)?;
        let statements = match operation {
            MigrationOperation::CreateModel { model } => create_table_statements(&model.table),
            MigrationOperation::CreateTable { table } => create_table_statements(table),
            MigrationOperation::DropTable { name } => {
                vec![format!("DROP TABLE {}", quote_identifier(name))]
            }
            MigrationOperation::RenameTable { old_name, new_name } => vec![format!(
                "ALTER TABLE {} RENAME TO {}",
                quote_identifier(old_name),
                quote_identifier(new_name)
            )],
            MigrationOperation::RenameColumn {
                table,
                old_name,
                new_name,
            } => vec![format!(
                "ALTER TABLE {} RENAME COLUMN {} TO {}",
                quote_identifier(table),
                quote_identifier(old_name),
                quote_identifier(new_name)
            )],
            _ if Self::rebuild_table_name(operation).is_some() => rebuild_table_statements(
                Self::rebuild_table_name(operation).expect("operation requires a rebuild"),
                before,
                &resulting_state,
                operation,
            )?,
            MigrationOperation::RunSql { forward, .. } => vec![forward.clone()],
            _ => unreachable!("all non-rebuild migration operations are handled above"),
        };
        Ok(SqliteOperationPlan {
            statements,
            resulting_state,
        })
    }
}

fn rebuild_table_statements(
    table_name: &str,
    before: &ProjectState,
    after: &ProjectState,
    operation: &MigrationOperation,
) -> Result<Vec<String>, MigrationError> {
    let old_table = &before.tables[table_index(before, table_name)?];
    let new_table = &after.tables[table_index(after, table_name)?];
    let temporary_name = format!("__che_migration_new_{table_name}");
    let mut temporary = new_table.clone();
    temporary.name = temporary_name.clone();
    let mut statements = vec![create_table_sql(&temporary)];

    let old_columns: BTreeSet<_> = old_table
        .columns
        .iter()
        .map(|column| column.name.as_str())
        .collect();
    let copied_columns: Vec<_> = new_table
        .columns
        .iter()
        .filter_map(|column| {
            copy_source_column(column, operation, &old_columns)
                .map(|source| (column.name.as_str(), source))
        })
        .collect();
    if !copied_columns.is_empty() {
        let destinations = copied_columns
            .iter()
            .map(|(destination, _)| quote_identifier(destination))
            .collect::<Vec<_>>()
            .join(", ");
        let sources = copied_columns
            .iter()
            .map(|(_, source)| quote_identifier(source))
            .collect::<Vec<_>>()
            .join(", ");
        statements.push(format!(
            "INSERT INTO {} ({destinations}) SELECT {sources} FROM {}",
            quote_identifier(&temporary_name),
            quote_identifier(table_name)
        ));
    }
    statements.push(format!("DROP TABLE {}", quote_identifier(table_name)));
    statements.push(format!(
        "ALTER TABLE {} RENAME TO {}",
        quote_identifier(&temporary_name),
        quote_identifier(table_name)
    ));
    statements.extend(index_statements(new_table));
    Ok(statements)
}

fn copy_source_column<'a>(
    new_column: &'a ColumnState,
    operation: &'a MigrationOperation,
    old_columns: &BTreeSet<&str>,
) -> Option<&'a str> {
    if let MigrationOperation::RenameColumn {
        old_name, new_name, ..
    } = operation
    {
        if new_column.name == *new_name && old_columns.contains(old_name.as_str()) {
            return Some(old_name);
        }
    }
    old_columns
        .contains(new_column.name.as_str())
        .then_some(new_column.name.as_str())
}

fn create_table_statements(table: &TableState) -> Vec<String> {
    let mut statements = vec![create_table_sql(table)];
    statements.extend(index_statements(table));
    statements
}

fn create_table_sql(table: &TableState) -> String {
    let columns = table
        .columns
        .iter()
        .map(column_definition_sql)
        .chain(table.unique_constraints.iter().map(|columns| {
            format!(
                "UNIQUE ({})",
                columns
                    .iter()
                    .map(|column| quote_identifier(column))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }))
        .collect::<Vec<_>>()
        .join(", ");
    format!("CREATE TABLE {} ({columns})", quote_identifier(&table.name))
}

fn index_statements(table: &TableState) -> Vec<String> {
    let mut statements = Vec::new();
    for (position, columns) in table.indexes.iter().enumerate() {
        statements.push(format!(
            "CREATE INDEX {} ON {} ({})",
            quote_identifier(&format!("{}_idx_{position}", table.name)),
            quote_identifier(&table.name),
            columns
                .iter()
                .map(|column| quote_identifier(column))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    statements
}

fn column_definition_sql(column: &ColumnState) -> String {
    let mut sql = format!(
        "{} {}",
        quote_identifier(&column.name),
        sqlite_column_type(column.column_type)
    );
    if column.primary_key {
        sql.push_str(" PRIMARY KEY");
    } else if !column.nullable {
        sql.push_str(" NOT NULL");
    }
    if column.unique {
        sql.push_str(" UNIQUE");
    }
    if column.auto_now_add || column.auto_now {
        if column.default.is_none() {
            sql.push_str(" DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))");
        }
    }
    if let Some(default) = &column.default {
        sql.push_str(" DEFAULT ");
        sql.push_str(default);
    }
    if let Some(check) = &column.check {
        sql.push_str(" CHECK (");
        sql.push_str(check);
        sql.push(')');
    }
    if let Some(choices) = &column.choices {
        sql.push_str(" CHECK (");
        sql.push_str(&quote_identifier(&column.name));
        sql.push_str(" IN (");
        sql.push_str(
            &choices
                .iter()
                .map(|choice| format!("'{}'", choice.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(", "),
        );
        sql.push_str("))");
    }
    if let Some(reference) = &column.references {
        sql.push_str(" REFERENCES ");
        sql.push_str(&reference.target);
        if let Some(on_delete) = &reference.on_delete {
            sql.push_str(" ON DELETE ");
            sql.push_str(&on_delete.to_ascii_uppercase());
        }
    }
    sql
}

fn sqlite_column_type(column_type: ColumnType) -> &'static str {
    match column_type {
        ColumnType::Integer => "INTEGER",
        ColumnType::Text => "TEXT",
        ColumnType::Binary => "BLOB",
        ColumnType::Boolean => "INTEGER",
        ColumnType::DateTime => "TEXT",
    }
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(app: &str, name: &str) -> MigrationId {
        MigrationId {
            app: app.into(),
            name: name.into(),
        }
    }

    fn migration(id: MigrationId, dependencies: Vec<MigrationId>) -> Migration {
        Migration {
            id,
            dependencies,
            operations: vec![],
            checksum: "test".into(),
        }
    }

    fn column(name: &str) -> ColumnState {
        ColumnState {
            name: name.into(),
            column_type: ColumnType::Integer,
            nullable: false,
            primary_key: false,
            unique: false,
            default: None,
            check: None,
            choices: None,
            references: None,
            auto_now_add: false,
            auto_now: false,
        }
    }

    #[derive(Debug, crate::Model)]
    #[orm(table = "legacy_diff")]
    struct LegacyDiffModel {
        #[orm(primary_key)]
        id: i64,
    }

    #[test]
    fn graph_orders_and_replays_migrations_deterministically() {
        let graph = MigrationGraph::new(vec![
            Migration {
                id: id("blog", "0002_title"),
                dependencies: vec![id("blog", "0001_initial")],
                checksum: "b".into(),
                operations: vec![MigrationOperation::AddColumn {
                    table: "posts".into(),
                    column: ColumnState {
                        name: "title".into(),
                        column_type: ColumnType::Text,
                        nullable: false,
                        primary_key: false,
                        unique: false,
                        default: None,
                        check: None,
                        choices: None,
                        references: None,
                        auto_now_add: false,
                        auto_now: false,
                    },
                }],
            },
            Migration {
                id: id("blog", "0001_initial"),
                dependencies: vec![],
                checksum: "a".into(),
                operations: vec![MigrationOperation::CreateTable {
                    table: TableState {
                        name: "posts".into(),
                        columns: vec![],
                        unique_constraints: vec![],
                        indexes: vec![],
                    },
                }],
            },
        ])
        .unwrap();
        assert_eq!(
            graph
                .ordered()
                .map(|migration| migration.id.name.as_str())
                .collect::<Vec<_>>(),
            ["0001_initial", "0002_title"]
        );
        assert_eq!(
            graph.replay(ProjectState::default()).unwrap().tables()[0].columns[0].name,
            "title"
        );
    }

    #[test]
    fn graph_rejects_invalid_dependencies() {
        assert!(matches!(
            MigrationGraph::new(vec![migration(id("a", "one"), vec![id("a", "missing")])]),
            Err(MigrationError::MissingDependency { .. })
        ));
        assert!(matches!(
            MigrationGraph::new(vec![
                migration(id("a", "one"), vec![id("a", "two")]),
                migration(id("a", "two"), vec![id("a", "one")])
            ]),
            Err(MigrationError::CyclicDependency(_))
        ));
        assert!(matches!(
            MigrationGraph::new(vec![
                migration(id("a", "one"), vec![]),
                migration(id("a", "one"), vec![])
            ]),
            Err(MigrationError::DuplicateMigration(_))
        ));
    }

    #[test]
    fn run_sql_applies_its_state_operations() {
        let mut state = ProjectState::default();
        MigrationOperation::RunSql {
            forward: "CREATE TABLE notes".into(),
            reverse: Some("DROP TABLE notes".into()),
            state_operations: vec![MigrationOperation::CreateTable {
                table: TableState {
                    name: "notes".into(),
                    columns: vec![],
                    unique_constraints: vec![],
                    indexes: vec![],
                },
            }],
        }
        .state_forwards(&mut state)
        .unwrap();
        assert_eq!(state.tables()[0].name, "notes");
    }

    #[test]
    fn renames_keep_constraints_and_foreign_keys_in_sync() {
        let mut parent_id = column("id");
        parent_id.primary_key = true;
        let mut child_parent_id = column("parent_id");
        child_parent_id.references = Some(ForeignKeyState {
            target: "parents(id)".into(),
            on_delete: Some("cascade".into()),
        });
        let mut state = ProjectState {
            tables: vec![
                TableState {
                    name: "parents".into(),
                    columns: vec![parent_id, column("title")],
                    unique_constraints: vec![vec!["title".into()]],
                    indexes: vec![vec!["title".into()]],
                },
                TableState {
                    name: "children".into(),
                    columns: vec![column("id"), child_parent_id],
                    unique_constraints: vec![],
                    indexes: vec![],
                },
            ],
            model_keys: BTreeMap::new(),
        };

        MigrationOperation::RenameTable {
            old_name: "parents".into(),
            new_name: "accounts".into(),
        }
        .state_forwards(&mut state)
        .unwrap();
        MigrationOperation::RenameColumn {
            table: "accounts".into(),
            old_name: "id".into(),
            new_name: "account_id".into(),
        }
        .state_forwards(&mut state)
        .unwrap();
        MigrationOperation::RenameColumn {
            table: "accounts".into(),
            old_name: "title".into(),
            new_name: "name".into(),
        }
        .state_forwards(&mut state)
        .unwrap();

        assert_eq!(state.tables()[0].unique_constraints, vec![vec!["name"]]);
        assert_eq!(state.tables()[0].indexes, vec![vec!["name"]]);
        assert_eq!(
            state.tables()[1].columns[1]
                .references
                .as_ref()
                .unwrap()
                .target,
            "accounts(account_id)"
        );
    }

    #[test]
    fn removal_rejects_referenced_tables_and_columns() {
        let mut child_parent_id = column("parent_id");
        child_parent_id.references = Some(ForeignKeyState {
            target: "parents(id)".into(),
            on_delete: None,
        });
        let mut state = ProjectState {
            tables: vec![
                TableState {
                    name: "parents".into(),
                    columns: vec![column("id")],
                    unique_constraints: vec![],
                    indexes: vec![],
                },
                TableState {
                    name: "children".into(),
                    columns: vec![child_parent_id],
                    unique_constraints: vec![],
                    indexes: vec![],
                },
            ],
            model_keys: BTreeMap::new(),
        };

        assert!(matches!(
            MigrationOperation::RemoveColumn {
                table: "parents".into(),
                column: "id".into(),
            }
            .state_forwards(&mut state),
            Err(MigrationError::DependentColumn { .. })
        ));
        assert!(matches!(
            MigrationOperation::DropTable {
                name: "parents".into(),
            }
            .state_forwards(&mut state),
            Err(MigrationError::DependentTable { .. })
        ));
    }

    #[test]
    fn calculated_checksum_detects_modified_migration_content() {
        let mut migration = Migration::new(
            id("tasks", "0001_initial"),
            vec![],
            vec![MigrationOperation::RunSql {
                forward: "CREATE TABLE tasks (id INTEGER PRIMARY KEY);".into(),
                reverse: Some("DROP TABLE tasks;".into()),
                state_operations: vec![],
            }],
        );
        assert!(migration.checksum.starts_with("v1:"));
        assert!(MigrationGraph::new(vec![migration.clone()]).is_ok());

        let MigrationOperation::RunSql { forward, .. } = &mut migration.operations[0] else {
            unreachable!();
        };
        *forward = "CREATE TABLE tasks (id INTEGER PRIMARY KEY, name TEXT);".into();
        assert!(matches!(
            MigrationGraph::new(vec![migration]),
            Err(MigrationError::InvalidChecksum(_))
        ));
    }

    #[test]
    fn review_regressions_forward_plan_and_state() {
        let a = migration(id("a", "0001"), vec![]);
        let b = migration(id("b", "0001"), vec![a.id.clone()]);
        let graph = MigrationGraph::new(vec![a.clone(), b.clone()]).unwrap();
        assert_eq!(graph.app_heads("a").len(), 1);
        assert!(
            graph
                .plan_to(&BTreeSet::from([a.id.clone(), b.id.clone()]), Some(&a.id))
                .unwrap()
                .is_empty()
        );
        let independent = migration(id("c", "0001"), vec![]);
        let graph = MigrationGraph::new(vec![a.clone(), independent.clone()]).unwrap();
        assert!(
            graph
                .plan_forward(
                    &BTreeSet::from([a.id.clone(), independent.id.clone()]),
                    Some(&a.id)
                )
                .unwrap()
                .is_empty()
        );
        let graph = MigrationGraph::new(vec![Migration::new(
            id("bad", "0001"),
            vec![],
            vec![MigrationOperation::DropTable {
                name: "missing".into(),
            }],
        )])
        .unwrap();
        assert!(graph.plan_to(&BTreeSet::new(), None).is_err());
        let mut checked = column("value");
        checked.check = Some("value > 0".into());
        let model = ModelState {
            key: ModelKey {
                app: "a".into(),
                name: "Item".into(),
            },
            table: TableState {
                name: "items".into(),
                columns: vec![checked],
                indexes: vec![],
                unique_constraints: vec![],
            },
        };
        let mut state = ProjectState::default();
        MigrationOperation::CreateModel {
            model: model.clone(),
        }
        .state_forwards(&mut state)
        .unwrap();
        assert_eq!(state.models(), vec![model.clone()]);
        let graph = MigrationGraph::new(vec![Migration::new(
            id("a", "0001"),
            vec![],
            vec![MigrationOperation::CreateModel {
                model: model.clone(),
            }],
        )])
        .unwrap();
        assert_eq!(
            graph.replay(ProjectState::default()).unwrap().models(),
            state.models()
        );
        assert!(
            MigrationOperation::RenameColumn {
                table: "items".into(),
                old_name: "value".into(),
                new_name: "renamed".into()
            }
            .state_forwards(&mut state)
            .is_err()
        );
        assert_eq!(state.models(), vec![model]);
    }

    #[test]
    fn diff_uses_model_keys_and_does_not_guess_field_renames() {
        let key = ModelKey {
            app: "items".into(),
            name: "Item".into(),
        };
        let before_model = ModelState {
            key: key.clone(),
            table: TableState {
                name: "items".into(),
                columns: vec![column("id"), column("old_title")],
                indexes: vec![],
                unique_constraints: vec![],
            },
        };
        let after_model = ModelState {
            key: key.clone(),
            table: TableState {
                name: "catalog_items".into(),
                columns: vec![column("id"), column("title")],
                indexes: vec![vec!["title".into()]],
                unique_constraints: vec![],
            },
        };
        let create = MigrationOperation::CreateModel {
            model: before_model.clone(),
        };
        let mut before = ProjectState::default();
        create.state_forwards(&mut before).unwrap();
        let mut after = ProjectState::default();
        MigrationOperation::CreateModel {
            model: after_model.clone(),
        }
        .state_forwards(&mut after)
        .unwrap();
        assert_eq!(
            before.diff(&after).unwrap(),
            vec![
                StateChange::RenameTable {
                    key: key.clone(),
                    old_name: "items".into(),
                    new_name: "catalog_items".into()
                },
                StateChange::RemoveColumn {
                    key: key.clone(),
                    table: "catalog_items".into(),
                    column: column("old_title")
                },
                StateChange::AddColumn {
                    key: key.clone(),
                    table: "catalog_items".into(),
                    column: column("title")
                },
                StateChange::AddIndex {
                    key,
                    table: "catalog_items".into(),
                    columns: vec!["title".into()]
                },
            ]
        );
        let legacy = ProjectState::from_schema(&SchemaSet::new().model::<LegacyDiffModel>());
        assert!(legacy.diff(&after).is_err());
    }

    #[test]
    fn alter_column_and_constraints_update_historical_state() {
        let initial = TableState {
            name: "tasks".into(),
            columns: vec![column("id"), column("title")],
            unique_constraints: vec![],
            indexes: vec![],
        };
        let old_title = column("title");
        let mut new_title = old_title.clone();
        new_title.nullable = true;
        let graph = MigrationGraph::new(vec![Migration::new(
            id("tasks", "0001_initial"),
            vec![],
            vec![
                MigrationOperation::CreateTable { table: initial },
                MigrationOperation::AddIndex {
                    table: "tasks".into(),
                    columns: vec!["title".into()],
                },
                MigrationOperation::AddUniqueConstraint {
                    table: "tasks".into(),
                    columns: vec!["title".into()],
                },
                MigrationOperation::AlterColumn {
                    table: "tasks".into(),
                    old_column: old_title,
                    new_column: new_title,
                },
            ],
        )])
        .unwrap();
        let state = graph.replay(ProjectState::default()).unwrap();
        let table = &state.tables()[0];
        assert!(table.columns[1].nullable);
        assert_eq!(table.indexes, vec![vec!["title"]]);
        assert_eq!(table.unique_constraints, vec![vec!["title"]]);
    }

    #[test]
    fn planner_only_moves_forward_and_rejects_branches() {
        let first = migration(id("tasks", "0001_initial"), vec![]);
        let second = migration(id("tasks", "0002_title"), vec![first.id.clone()]);
        let branch = migration(id("tasks", "0002_description"), vec![first.id.clone()]);
        let graph =
            MigrationGraph::new(vec![first.clone(), second.clone(), branch.clone()]).unwrap();
        let applied = BTreeSet::from([first.id.clone()]);
        assert!(graph.plan_to(&applied, Some(&second.id)).is_err());
        assert_eq!(graph.app_heads("tasks").len(), 2);
        let graph = MigrationGraph::new(vec![first.clone(), second.clone()]).unwrap();

        assert_eq!(
            graph
                .plan_to(&applied, Some(&second.id))
                .unwrap()
                .iter()
                .map(|step| step.id.name.as_str())
                .collect::<Vec<_>>(),
            vec!["0002_title"]
        );
        assert!(
            graph
                .plan_to(
                    &BTreeSet::from([first.id.clone(), second.id.clone()]),
                    Some(&first.id)
                )
                .is_err()
        );
        assert_eq!(
            graph
                .app_heads("tasks")
                .iter()
                .map(|migration| migration.id.name.as_str())
                .collect::<Vec<_>>(),
            vec!["0002_title"]
        );
    }

    #[test]
    fn planner_rejects_inconsistent_history_and_irreversible_rollback() {
        let first = migration(id("tasks", "0001_initial"), vec![]);
        let irreversible = Migration::new(
            id("tasks", "0002_data"),
            vec![first.id.clone()],
            vec![MigrationOperation::RunSql {
                forward: "UPDATE tasks SET title = trim(title);".into(),
                reverse: None,
                state_operations: vec![],
            }],
        );
        let graph = MigrationGraph::new(vec![first.clone(), irreversible.clone()]).unwrap();
        assert!(matches!(
            graph.plan_to(&BTreeSet::from([irreversible.id.clone()]), None),
            Err(MigrationError::InconsistentAppliedHistory { .. })
        ));
        assert!(matches!(
            graph.plan_to(
                &BTreeSet::from([first.id.clone(), irreversible.id.clone()]),
                Some(&first.id)
            ),
            Err(MigrationError::InvalidState(_))
        ));
    }

    #[test]
    fn sqlite_editor_rebuilds_a_table_and_preserves_existing_rows() {
        let create = MigrationOperation::CreateTable {
            table: TableState {
                name: "items".into(),
                columns: vec![column("id")],
                unique_constraints: vec![],
                indexes: vec![],
            },
        };
        let initial = SqliteSchemaEditor::plan(&create, &ProjectState::default()).unwrap();
        let connection = crate::rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(&(initial.statements.join("; ") + ";"))
            .unwrap();
        connection
            .execute("INSERT INTO items (id) VALUES (7)", [])
            .unwrap();

        let mut name = column("name");
        name.column_type = ColumnType::Text;
        name.default = Some("'unknown'".into());
        let add_column = MigrationOperation::AddColumn {
            table: "items".into(),
            column: name,
        };
        let rebuild = SqliteSchemaEditor::plan(&add_column, &initial.resulting_state).unwrap();
        assert!(rebuild
            .statements
            .iter()
            .any(|statement| statement.starts_with("INSERT INTO \"__che_migration_new_items\"")));
        connection
            .execute_batch(&(rebuild.statements.join("; ") + ";"))
            .unwrap();
        let row: (i64, String) = connection
            .query_row("SELECT id, name FROM items", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(row, (7, "unknown".into()));
    }

    #[test]
    fn state_changes_become_replayable_forward_operations() {
        let key = ModelKey {
            app: "tasks".into(),
            name: "Task".into(),
        };
        let model = ModelState {
            key: key.clone(),
            table: TableState {
                name: "tasks".into(),
                columns: vec![column("id")],
                unique_constraints: vec![],
                indexes: vec![],
            },
        };
        let mut expected = model.clone();
        expected.table.columns.push(column("title"));
        let changes = vec![
            StateChange::CreateModel(model.clone()),
            StateChange::AddColumn {
                key,
                table: "tasks".into(),
                column: column("title"),
            },
        ];
        let operations = operations_from_changes(changes);
        let mut state = ProjectState::default();
        for operation in &operations {
            operation.state_forwards(&mut state).unwrap();
        }
        assert_eq!(state.models(), vec![expected]);
        assert_eq!(state.tables()[0].columns[1].name, "title");
    }

    #[test]
    fn rust_writer_is_deterministic_and_uses_migration_constructor() {
        let mut title = column("title");
        title.default = Some("'quoted value'".into());
        title.references = Some(ForeignKeyState {
            target: "accounts(id)".into(),
            on_delete: Some("cascade".into()),
        });
        let migration = Migration::new(
            id("tasks", "0002_title"),
            vec![id("accounts", "0001_initial")],
            vec![MigrationOperation::RunSql {
                forward: "UPDATE tasks SET title = 'ready';".into(),
                reverse: None,
                state_operations: vec![MigrationOperation::AddColumn {
                    table: "tasks".into(),
                    column: title,
                }],
            }],
        );
        let source = render_migration_rust(&migration);
        assert_eq!(source, render_migration_rust(&migration));
        assert!(source.contains("Migration::new("));
        assert!(source.contains("accounts"));
        assert!(source.contains("UPDATE tasks SET title"));
        assert!(!source.contains(&migration.checksum));
    }
}
