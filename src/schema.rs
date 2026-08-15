use std::marker::PhantomData;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// SQL column types supported by the schema compiler.
pub enum ColumnType {
    Integer,
    Text,
    Boolean,
    DateTime,
}

/// Maps a Rust field type to a SQL column type.
pub trait ColumnTypeOf {
    fn column_type() -> ColumnType;
    fn nullable() -> bool {
        false
    }
}

impl ColumnTypeOf for i64 {
    fn column_type() -> ColumnType {
        ColumnType::Integer
    }
}

impl ColumnTypeOf for String {
    fn column_type() -> ColumnType {
        ColumnType::Text
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Foreign key metadata attached to a column.
pub struct ForeignKey {
    pub target: &'static str,
    pub on_delete: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Metadata for one table column.
pub struct ColumnSchema {
    pub name: &'static str,
    pub column_type: ColumnType,
    pub nullable: bool,
    pub primary_key: bool,
    pub unique: bool,
    pub default: Option<&'static str>,
    pub check: Option<&'static str>,
    pub references: Option<ForeignKey>,
    pub auto_now_add: bool,
    pub auto_now: bool,
}

impl ColumnSchema {
    pub fn new(name: &'static str, column_type: ColumnType, nullable: bool) -> Self {
        Self {
            name,
            column_type,
            nullable,
            primary_key: false,
            unique: false,
            default: None,
            check: None,
            references: None,
            auto_now_add: false,
            auto_now: false,
        }
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

/// A collection of model schemas exported as one Atlas-compatible SQL schema.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SchemaSet {
    tables: Vec<TableSchema>,
}

impl SchemaSet {
    /// Creates an empty schema collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a model schema in dependency order.
    pub fn model<M: crate::Model>(mut self) -> Self {
        self.tables.push(M::schema());
        self
    }

    /// Merges another application schema after the current tables.
    pub fn merge(mut self, other: Self) -> Self {
        self.tables.extend(other.tables);
        self
    }

    /// Exports the desired schema as semicolon-delimited SQL.
    pub fn to_sql<D: crate::SqlDialect>(&self) -> String {
        let mut statements = Vec::new();
        for table in &self.tables {
            let compiled = crate::SqlCompiler::<D>::compile_schema(table);
            statements.push(compiled.table);
            statements.extend(compiled.indexes);
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
}

impl AppRegistry {
    /// Creates an empty application registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one application and appends its models to the schema.
    pub fn register<A: AppConfig>(mut self) -> Self {
        self.apps.push(A::name());
        self.schema = self.schema.merge(A::schema());
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

    /// Exports all registered models as Atlas-compatible SQL.
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
