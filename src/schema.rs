use std::marker::PhantomData;

pub(crate) fn schema_object_name(table: &str, kind: &str, parts: &[&str]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for part in std::iter::once(table)
        .chain(std::iter::once(kind))
        .chain(parts.iter().copied())
    {
        for byte in part.bytes().chain(std::iter::once(0)) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
    }
    let prefix = format!("{table}_{kind}");
    let mut end = prefix.len().min(46);
    while !prefix.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}_{hash:016x}", &prefix[..end])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// SQL column types supported by the schema compiler.
pub enum ColumnType {
    Integer,
    Real,
    Text,
    Binary,
    Boolean,
    DateTime,
    Uuid,
}

/// Maps a Rust field type to a SQL column type.
pub trait ColumnTypeOf {
    fn column_type() -> ColumnType;
    fn nullable() -> bool {
        false
    }
    fn choices() -> Option<Vec<&'static str>> {
        None
    }
}

impl ColumnTypeOf for i64 {
    fn column_type() -> ColumnType {
        ColumnType::Integer
    }
}

impl ColumnTypeOf for uuid::Uuid {
    fn column_type() -> ColumnType {
        ColumnType::Uuid
    }
}

impl ColumnTypeOf for f64 {
    fn column_type() -> ColumnType {
        ColumnType::Real
    }
}

impl ColumnTypeOf for String {
    fn column_type() -> ColumnType {
        ColumnType::Text
    }
}

impl ColumnTypeOf for Vec<u8> {
    fn column_type() -> ColumnType {
        ColumnType::Binary
    }
}

impl ColumnTypeOf for bool {
    fn column_type() -> ColumnType {
        ColumnType::Boolean
    }
}

impl ColumnTypeOf for time::OffsetDateTime {
    fn column_type() -> ColumnType {
        ColumnType::DateTime
    }
}

impl<T: ColumnTypeOf> ColumnTypeOf for Option<T> {
    fn column_type() -> ColumnType {
        T::column_type()
    }
    fn nullable() -> bool {
        true
    }
    fn choices() -> Option<Vec<&'static str>> {
        T::choices()
    }
}

/// A string-backed enum that can be stored in a database column.
pub trait DbEnum: Sized {
    const VALUES: &'static [&'static str];

    fn as_str(&self) -> &'static str;
    fn from_str(value: &str) -> Option<Self>;
}

impl<T: DbEnum> ColumnTypeOf for T {
    fn column_type() -> ColumnType {
        ColumnType::Text
    }

    fn choices() -> Option<Vec<&'static str>> {
        Some(T::VALUES.to_vec())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Foreign key metadata attached to a column.
pub struct ForeignKey {
    target: String,
    on_delete: Option<&'static str>,
}

impl ForeignKey {
    pub fn new(target: impl Into<String>, on_delete: Option<&'static str>) -> Self {
        Self {
            target: target.into(),
            on_delete,
        }
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn target_parts(&self) -> Option<(&str, &str)> {
        self.target
            .split_once('(')
            .and_then(|(table, column)| column.strip_suffix(')').map(|column| (table, column)))
    }

    pub fn on_delete(&self) -> Option<&'static str> {
        self.on_delete
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Metadata for one table column.
pub struct ColumnSchema {
    /// Rust field name used to identify this column in migration state.
    pub field_name: &'static str,
    /// Physical column name used in SQL.
    pub name: &'static str,
    pub column_type: ColumnType,
    pub nullable: bool,
    pub primary_key: bool,
    pub unique: bool,
    pub default: Option<&'static str>,
    pub check: Option<&'static str>,
    pub choices: Option<Vec<&'static str>>,
    pub references: Option<ForeignKey>,
    pub auto_now_add: bool,
    pub auto_now: bool,
}

impl ColumnSchema {
    pub fn new(name: &'static str, column_type: ColumnType, nullable: bool) -> Self {
        Self {
            field_name: name,
            name,
            column_type,
            nullable,
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

    /// Associates a Rust field name with a physical SQL column name.
    pub fn with_field_name(mut self, field_name: &'static str) -> Self {
        self.field_name = field_name;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Complete model table metadata used for DDL generation.
pub struct TableSchema {
    pub name: &'static str,
    pub columns: Vec<ColumnSchema>,
    pub unique_constraints: Vec<Vec<&'static str>>,
    pub indexes: Vec<Vec<&'static str>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Errors found in model-generated schema metadata.
pub enum SchemaError {
    InvalidIdentifier(String),
    DuplicateTable(String),
    DuplicateColumn { table: String, column: String },
    UnknownColumn { table: String, column: String },
    InvalidForeignKey(String),
    InvalidOnDelete(String),
    IncompatibleSyncRoles,
}

impl TableSchema {
    /// Validates identifiers, indexes, unique constraints and foreign keys.
    pub fn validate(&self) -> Result<(), SchemaError> {
        validate_identifier(self.name)?;
        let mut columns = std::collections::HashSet::new();
        for column in &self.columns {
            validate_identifier(column.name)?;
            if !columns.insert(column.name) {
                return Err(SchemaError::DuplicateColumn {
                    table: self.name.into(),
                    column: column.name.into(),
                });
            }
            if let Some(reference) = &column.references {
                validate_reference(reference.target())?;
                if let Some(action) = reference.on_delete()
                    && !matches!(
                        action.to_ascii_lowercase().as_str(),
                        "cascade" | "restrict" | "no action" | "set null" | "set default"
                    )
                {
                    return Err(SchemaError::InvalidOnDelete(action.into()));
                }
            }
        }
        for group in self.unique_constraints.iter().chain(self.indexes.iter()) {
            for column in group {
                validate_identifier(column)?;
                if !columns.contains(column) {
                    return Err(SchemaError::UnknownColumn {
                        table: self.name.into(),
                        column: (*column).into(),
                    });
                }
            }
        }
        Ok(())
    }
}

impl SchemaSet {
    /// Validates every registered table and rejects duplicate table names.
    pub fn validate(&self) -> Result<(), SchemaError> {
        if let Some(first) = self.sync_models.first() {
            if self
                .sync_models
                .iter()
                .any(|model| model.role != first.role)
            {
                return Err(SchemaError::IncompatibleSyncRoles);
            }
        }
        let mut tables = std::collections::HashSet::new();
        for table in &self.tables {
            table.validate()?;
            if !tables.insert(table.name) {
                return Err(SchemaError::DuplicateTable(table.name.into()));
            }
        }
        for table in &self.tables {
            for column in &table.columns {
                let Some(reference) = &column.references else {
                    continue;
                };
                let (target_table, target_column) = reference
                    .target
                    .split_once('(')
                    .and_then(|(table, column)| {
                        column.strip_suffix(')').map(|column| (table, column))
                    })
                    .ok_or_else(|| SchemaError::InvalidForeignKey(reference.target().into()))?;
                let target = self
                    .tables
                    .iter()
                    .find(|candidate| candidate.name == target_table)
                    .ok_or_else(|| SchemaError::InvalidForeignKey(reference.target().into()))?;
                if !target
                    .columns
                    .iter()
                    .any(|candidate| candidate.name == target_column)
                {
                    return Err(SchemaError::UnknownColumn {
                        table: target_table.into(),
                        column: target_column.into(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Exports SQL and returns schema validation errors instead of panicking.
    pub fn try_to_sql<D: crate::SqlDialect>(&self) -> Result<String, SchemaError> {
        self.validate()?;
        Ok(self.to_sql_unchecked::<D>())
    }
}

pub(crate) fn validate_identifier(identifier: &str) -> Result<(), SchemaError> {
    let mut chars = identifier.chars();
    let valid_start = chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic());
    if valid_start && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err(SchemaError::InvalidIdentifier(identifier.into()))
    }
}

fn validate_reference(reference: &str) -> Result<(), SchemaError> {
    let Some((table, column)) = reference.split_once('(') else {
        return Err(SchemaError::InvalidForeignKey(reference.into()));
    };
    let Some(column) = column.strip_suffix(')') else {
        return Err(SchemaError::InvalidForeignKey(reference.into()));
    };
    validate_identifier(table)?;
    validate_identifier(column)
}

/// A collection of model schemas exported as one SQL schema.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SchemaSet {
    tables: Vec<TableSchema>,
    model_names: Vec<&'static str>,
    sync_models: Vec<crate::SyncRegistration>,
}

impl SchemaSet {
    /// Creates an empty schema collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns registered tables in schema order.
    pub fn tables(&self) -> &[TableSchema] {
        &self.tables
    }

    /// Returns model names parallel to [`SchemaSet::tables`].
    pub fn model_names(&self) -> &[&'static str] {
        &self.model_names
    }

    /// Adds a model schema in dependency order.
    pub fn model<M: crate::Model>(mut self) -> Self {
        self.tables.push(M::schema());
        self.model_names.push(M::model_name());
        self
    }

    /// Register the local editable projection and its confirmed server shadow.
    pub fn client_model<M: crate::SyncModel>(self) -> Self {
        self.sync_model::<M>(crate::ModelRole::Client)
    }

    /// Register authoritative server data, without a shadow table.
    pub fn server_model<M: crate::SyncModel>(self) -> Self {
        self.sync_model::<M>(crate::ModelRole::Server)
    }

    fn sync_model<M: crate::SyncModel>(mut self, role: crate::ModelRole) -> Self {
        let registration = crate::SyncRegistration::new::<M>(role);
        self.tables.push(registration.schema.clone());
        self.model_names.push(M::model_name());
        self.sync_models.push(registration);
        self
    }

    pub fn sync_models(&self) -> &[crate::SyncRegistration] {
        &self.sync_models
    }

    /// Merges another application schema after the current tables.
    pub fn merge(mut self, other: Self) -> Self {
        self.tables.extend(other.tables);
        self.model_names.extend(other.model_names);
        self.sync_models.extend(other.sync_models);
        self
    }

    /// Exports the desired schema as semicolon-delimited SQL.
    pub fn to_sql<D: crate::SqlDialect>(&self) -> String {
        self.validate().expect("invalid model schema");
        self.to_sql_unchecked::<D>()
    }

    fn to_sql_unchecked<D: crate::SqlDialect>(&self) -> String {
        let mut statements = Vec::new();
        for table in &self.tables {
            let compiled = crate::SqlCompiler::<D>::compile_schema(table);
            statements.push(compiled.table);
            statements.extend(compiled.indexes);
        }
        for operation in
            crate::sync_migration_operations(&self.sync_models).expect("validated sync roles")
        {
            if let crate::MigrationOperation::CreateModel { model } = operation {
                let compiled = crate::SqlCompiler::<D>::compile_table_state(&model.table);
                statements.push(compiled.table);
                statements.extend(compiled.indexes);
            }
        }
        statements.join(";\n") + if statements.is_empty() { "" } else { ";\n" }
    }
}

/// Application-level model registration, similar to a Django app config.
pub trait AppConfig {
    /// Stable application label used in diagnostics and registration.
    fn name() -> &'static str;

    /// Returns models owned by this application in dependency order.
    fn schema() -> SchemaSet;
}

/// Registry that combines schemas from multiple application modules.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AppRegistry {
    apps: Vec<&'static str>,
    schema: SchemaSet,
    app_schemas: Vec<(&'static str, SchemaSet)>,
}

impl AppRegistry {
    /// Creates an empty application registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one application and appends its models to the schema.
    pub fn register<A: AppConfig>(mut self) -> Self {
        self.apps.push(A::name());
        let schema = A::schema();
        self.schema = self.schema.merge(schema.clone());
        self.app_schemas.push((A::name(), schema));
        self
    }

    /// Returns registered application labels in registration order.
    pub fn apps(&self) -> &[&'static str] {
        &self.apps
    }

    /// Returns the combined schema.
    pub fn schema(&self) -> &SchemaSet {
        &self.schema
    }

    /// Returns schemas by owning app, preserving registration order for
    /// migration-state construction.
    pub fn app_schemas(&self) -> impl Iterator<Item = (&'static str, &SchemaSet)> {
        self.app_schemas
            .iter()
            .map(|(name, schema)| (*name, schema))
    }

    /// Exports all registered models as SQL.
    pub fn to_sql<D: crate::SqlDialect>(&self) -> String {
        self.schema.to_sql::<D>()
    }
}

#[derive(Debug, Clone)]
/// AST node for a `CREATE TABLE` statement.
pub struct CreateTableAst {
    pub schema: TableSchema,
}

/// Builder for a model's `CREATE TABLE` statement.
pub struct CreateTableQuery<M> {
    ast: CreateTableAst,
    _model: PhantomData<M>,
}

impl<M> CreateTableQuery<M> {
    pub fn new(schema: TableSchema) -> Self {
        Self {
            ast: CreateTableAst { schema },
            _model: PhantomData,
        }
    }

    pub fn into_ast(self) -> crate::QueryAst {
        crate::QueryAst::CreateTable(self.ast)
    }
}
