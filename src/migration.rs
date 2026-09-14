use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::{ColumnSchema, ColumnType, ForeignKey, SchemaSet, TableSchema};

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

/// The fully owned schema produced by replaying migrations.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProjectState {
    tables: Vec<TableState>,
}

impl ProjectState {
    /// Builds migration state from model-generated schema metadata.
    pub fn from_schema(schema: &SchemaSet) -> Self {
        Self {
            tables: schema.tables().iter().map(TableState::from).collect(),
        }
    }

    /// Returns tables in their current schema order.
    pub fn tables(&self) -> &[TableState] {
        &self.tables
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
    pub checksum: String,
}

/// A migration state transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationOperation {
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
    RunSql {
        forward: String,
        reverse: Option<String>,
        state_operations: Vec<MigrationOperation>,
    },
}

impl MigrationOperation {
    /// Applies this operation to the in-memory schema state.
    pub fn state_forwards(&self, state: &mut ProjectState) -> Result<(), MigrationError> {
        match self {
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
                let index = table_index(state, name)?;
                state.tables.remove(index);
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
                let table = table_mut(state, table)?;
                let index = column_index(table, column)?;
                table.columns.remove(index);
            }
            Self::RenameTable { old_name, new_name } => {
                if state.tables.iter().any(|table| table.name == *new_name) {
                    return Err(MigrationError::DuplicateTable(new_name.clone()));
                }
                table_mut(state, old_name)?.name = new_name.clone();
            }
            Self::RenameColumn {
                table,
                old_name,
                new_name,
            } => {
                let table = table_mut(state, table)?;
                if table.columns.iter().any(|column| column.name == *new_name) {
                    return Err(MigrationError::DuplicateColumn {
                        table: table.name.clone(),
                        column: new_name.clone(),
                    });
                }
                let index = column_index(table, old_name)?;
                table.columns[index].name = new_name.clone();
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

/// Errors while validating or replaying migrations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationError {
    DuplicateMigration(MigrationId),
    MissingDependency {
        migration: MigrationId,
        dependency: MigrationId,
    },
    CyclicDependency(Vec<MigrationId>),
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
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateMigration(id) => {
                write!(formatter, "duplicate migration {}:{}", id.app, id.name)
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
            Self::DuplicateTable(name) => write!(formatter, "duplicate table {name}"),
            Self::MissingTable(name) => write!(formatter, "missing table {name}"),
            Self::DuplicateColumn { table, column } => {
                write!(formatter, "duplicate column {table}.{column}")
            }
            Self::MissingColumn { table, column } => {
                write!(formatter, "missing column {table}.{column}")
            }
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

    /// Replays every migration from an initial schema state.
    pub fn replay(&self, mut state: ProjectState) -> Result<ProjectState, MigrationError> {
        for migration in self.ordered() {
            for operation in &migration.operations {
                operation.state_forwards(&mut state)?;
            }
        }
        Ok(state)
    }
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
}
