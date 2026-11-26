//! Generation metadata, deliberately separate from frozen migration operations.
use super::*;

// Namespace keys for identifiers that can differ in case, such as columns and views.
pub(super) fn physical_key(name: &str) -> String {
    #[cfg(feature = "sqlite")]
    {
        name.to_ascii_lowercase()
    }
    #[cfg(not(feature = "sqlite"))]
    {
        name.to_owned()
    }
}

pub(super) fn same_name(a: &str, b: &str) -> bool {
    #[cfg(feature = "sqlite")]
    {
        a.eq_ignore_ascii_case(b)
    }
    #[cfg(not(feature = "sqlite"))]
    {
        a == b
    }
}

pub(super) fn has_key(table: &TableState, column: &str) -> bool {
    table
        .columns
        .iter()
        .find(|c| same_name(&c.name, column))
        .is_some_and(|c| reference_target_ready(table, &c.name))
}

/// The keys an opaque table provides and the table/column pairs its CREATE SQL
/// requires. The author declares these; arbitrary SQL is never parsed for FKs.
#[derive(Clone, Debug, Default)]
pub struct SqlTableContract {
    pub keys: BTreeSet<String>,
    pub requires: BTreeSet<(String, String)>,
}

/// Policy for one generation run. Drop permissions are exact physical names,
/// not wildcards; a column is named `table.column`.
#[derive(Clone, Debug, Default)]
pub struct GenerationPolicy {
    /// Explicit (app, historical Rust name) to current Rust name mappings.
    pub rename_models: BTreeMap<(String, String), String>,
    /// Explicit (historical table, historical column) to current SQL column mappings.
    pub rename_columns: BTreeMap<(String, String), String>,
    pub allow_drop: BTreeSet<String>,
    pub sql_tables: BTreeMap<String, SqlTableContract>,
    pub external_keys: BTreeSet<(String, String)>,
    /// Compatibility with existing `references="table(column)"` applications.
    /// Set true to require explicit external keys.
    pub strict_external: bool,
}

pub(super) struct Catalog<'a> {
    desired: &'a ProjectState,
    pub policy: &'a GenerationPolicy,
    external: BTreeSet<(String, String)>,
}

fn sql_table<'a>(state: &'a ProjectState, name: &str) -> Option<&'a SchemaObjectState> {
    state
        .objects()
        .iter()
        .find(|o| o.kind == SchemaObjectKind::Table && o.name == name)
}

impl<'a> Catalog<'a> {
    pub fn new(
        old: &ProjectState,
        desired: &'a ProjectState,
        policy: &'a GenerationPolicy,
    ) -> Result<Self, MigrationError> {
        #[cfg(feature = "sqlite")]
        for state in [old, desired] {
            validate_sqlite_namespace(state)?;
        }
        old.validate()?;
        desired.validate()?;
        let owned = |name: &str| {
            [old, desired]
                .iter()
                .any(|s| s.model_keys.contains_key(name) || sql_table(s, name).is_some())
        };
        for (table, _) in policy.rename_columns.keys() {
            validate_migration_table_name(table)?;
        }
        for resource in &policy.allow_drop {
            let table = resource
                .split_once('.')
                .map_or(resource.as_str(), |(table, _)| table);
            validate_migration_table_name(table)?;
        }
        for table in policy.sql_tables.keys() {
            validate_migration_table_name(table)?;
            if sql_table(old, table).is_none() && sql_table(desired, table).is_none() {
                return Err(MigrationError::InvalidState(format!(
                    "SQL contract has no registered table: {table}"
                )));
            }
        }
        let mut external = BTreeSet::new();
        for (table, key) in &policy.external_keys {
            validate_migration_table_name(table)?;
            if owned(table) {
                return Err(MigrationError::InvalidState(format!(
                    "{table} is registered and cannot be external"
                )));
            }
            external.insert((table.clone(), physical_key(key)));
        }
        let references = [old, desired]
            .into_iter()
            .flat_map(|s| s.tables())
            .flat_map(|t| &t.columns)
            .filter_map(|c| c.references.as_ref()?.target_parts())
            .chain(
                policy
                    .sql_tables
                    .values()
                    .flat_map(|c| c.requires.iter().map(|(t, k)| (t.as_str(), k.as_str()))),
            );
        for (table, key) in references {
            validate_migration_table_name(table)?;
            if owned(table) {
                continue;
            }
            let target = (table.to_owned(), physical_key(key));
            if policy.strict_external && !external.contains(&target) {
                return Err(MigrationError::InvalidState(format!(
                    "unknown FK target {table}({key}); register its provider or declare an external key"
                )));
            }
            external.insert(target);
        }
        let catalog = Self {
            desired,
            policy,
            external,
        };
        catalog.validate_requirements(desired)?;
        Ok(catalog)
    }

    /// True only if the resource is available at this file boundary. Legacy SQL
    /// tables without key metadata still have a known provider; the DB checks
    /// their key at execution, just as it does for external requirements.
    pub fn available(&self, state: &ProjectState, table: &str, key: &str) -> bool {
        if let Some(t) = state.tables().iter().find(|t| t.name == table) {
            return has_key(t, key);
        }
        if sql_table(state, table).is_some() {
            return self
                .contract(table)
                .is_none_or(|c| c.keys.iter().any(|k| same_name(k, key)));
        }
        self.external
            .contains(&(table.to_owned(), physical_key(key)))
    }

    pub fn contract(&self, table: &str) -> Option<&SqlTableContract> {
        self.policy.sql_tables.get(table)
    }

    pub fn desired_model(&self, table: &str) -> Option<&ModelKey> {
        self.desired.model_keys.get(table)
    }

    /// Validate the complete desired catalog even when the schema diff is empty,
    /// and reuse the same check at intermediate file boundaries.
    pub fn validate_requirements(&self, state: &ProjectState) -> Result<(), MigrationError> {
        let require = |owner: &str, table: &str, key: &str| {
            if self.available(state, table, key) {
                Ok(())
            } else {
                Err(MigrationError::InvalidState(format!(
                    "{owner} requires {table}({key})"
                )))
            }
        };
        for model in state.tables() {
            for (table, key) in model
                .columns
                .iter()
                .filter_map(|c| c.references.as_ref()?.target_parts())
            {
                require(&model.name, table, key)?;
            }
        }
        for obj in state
            .objects()
            .iter()
            .filter(|o| o.kind == SchemaObjectKind::Table)
        {
            if let Some(contract) = self.contract(&obj.name) {
                for (table, key) in &contract.requires {
                    require(&obj.name, table, key)?;
                }
            }
        }
        Ok(())
    }

    #[cfg(feature = "postgres")]
    pub fn database_checked(
        &self,
        before: &ProjectState,
        ops: &[MigrationOperation],
    ) -> BTreeSet<String> {
        let mut names: BTreeSet<_> = self.external.iter().map(|(t, _)| t.clone()).collect();
        names.extend(
            before
                .objects()
                .iter()
                .filter(|o| o.kind == SchemaObjectKind::Table)
                .map(|o| o.name.clone()),
        );
        for op in ops {
            match op {
                MigrationOperation::CreateSchemaObject { object }
                    if object.kind == SchemaObjectKind::Table =>
                {
                    names.insert(object.name.clone());
                }
                MigrationOperation::DropSchemaObject {
                    kind: SchemaObjectKind::Table,
                    name,
                } => {
                    names.remove(name);
                }
                _ => {}
            }
        }
        names
    }

    pub fn check_intent(&self, changes: &[StateChange]) -> Result<(), MigrationError> {
        for change in changes {
            let resource = match change {
                StateChange::AddColumn { table, column, .. }
                    if !column.nullable
                        && column.default.is_none()
                        && !column.auto_now
                        && !column.auto_now_add =>
                {
                    return Err(MigrationError::NeedsManualMigration {
                        resource: format!("{table}.{}", column.name),
                        reason:
                            "adding a required column needs a default or a reviewed data backfill"
                                .into(),
                    });
                }
                StateChange::AlterColumn {
                    table,
                    old_column,
                    new_column,
                    ..
                } if old_column.column_type != new_column.column_type => {
                    return Err(MigrationError::NeedsManualMigration {
                        resource: format!("{table}.{}", new_column.name),
                        reason: "column type changed; provide a reviewed data conversion".into(),
                    });
                }
                StateChange::DeleteModel(m) => {
                    if let Some(new) = self.desired_model(&m.table.name)
                        && new.app == m.key.app
                        && new != &m.key
                        && !changes.iter().any(
                            |c| matches!(c, StateChange::RenameTable { key, .. } if key == new),
                        )
                    {
                        return Err(MigrationError::NeedsManualMigration {
                            resource: m.table.name.clone(),
                            reason: format!(
                                "same table is now owned by {}.{}; confirm model rename with --rename-model {}.{} {} or provide a manual replacement",
                                new.app, new.name, m.key.app, m.key.name, new.name
                            ),
                        });
                    }
                    m.table.name.clone()
                }
                StateChange::DeleteSchemaObject(o) if o.kind == SchemaObjectKind::Table => {
                    o.name.clone()
                }
                StateChange::RemoveColumn { table, column, .. } => {
                    format!("{table}.{}", column.name)
                }
                _ => continue,
            };
            if !self.policy.allow_drop.contains(&resource) {
                return Err(MigrationError::NeedsManualMigration {
                    reason: removal_advice(change, changes, &resource),
                    resource,
                });
            }
        }
        Ok(())
    }
}

fn removal_advice(change: &StateChange, changes: &[StateChange], resource: &str) -> String {
    let mut hints = Vec::new();
    for candidate in changes {
        match (change, candidate) {
            (
                StateChange::RemoveColumn { table, column, .. },
                StateChange::AddColumn {
                    table: dest,
                    column: next,
                    ..
                },
            ) if table == dest && column.column_type == next.column_type => {
                hints.push(format!("--rename-field {resource} {}", next.name));
            }
            (StateChange::DeleteModel(old), StateChange::CreateModel(new))
                if old.key.app == new.key.app && old.table.columns == new.table.columns =>
            {
                hints.push(format!(
                    "--rename-model {}.{} {}",
                    old.key.app, old.key.name, new.key.name
                ));
            }
            _ => {}
        }
    }
    if hints.is_empty() {
        format!("confirm removal with --drop {resource} or supply an explicit migration")
    } else {
        format!(
            "possible rename: {}; confirm removal with --drop {resource} only to discard existing data",
            hints.join(" or ")
        )
    }
}

#[cfg(feature = "sqlite")]
pub(super) fn validate_sqlite_namespace(state: &ProjectState) -> Result<(), MigrationError> {
    // SQLite folds ASCII case (including quoted names); tables and views share
    // a namespace. Do not change the stored spelling or historical checksums.
    let mut owners = BTreeMap::new();
    let mut register = |name: &str, owner: &str| {
        let description = format!("{owner}:{name}");
        if let Some(previous) = owners.insert(physical_key(name), description.clone()) {
            return Err(MigrationError::InvalidState(format!(
                "SQLite name collision: {previous} and {description}"
            )));
        }
        Ok(())
    };
    for table in state.tables() {
        let owner = state
            .model_keys
            .get(&table.name)
            .map_or("<history>", |k| k.app.as_str());
        register(&table.name, owner)?;
        let mut columns = BTreeMap::new();
        for column in &table.columns {
            if let Some(previous) = columns.insert(physical_key(&column.name), &column.name) {
                return Err(MigrationError::InvalidState(format!(
                    "SQLite column name collision on {}: {previous} and {}",
                    table.name, column.name
                )));
            }
        }
        for position in 0..table.indexes.len() {
            register(&index_name(&table.name, position), owner)?;
        }
    }
    for o in state
        .objects()
        .iter()
        .filter(|o| matches!(o.kind, SchemaObjectKind::Table | SchemaObjectKind::View))
    {
        register(&o.name, &o.app)?;
    }
    Ok(())
}
