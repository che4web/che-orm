use crate::Registry;
use che_orm::{
    ColumnState, ColumnType, MigrationError, MigrationOperation, ModelKey, ModelState,
    ProjectState, TableState,
};

#[derive(Debug, Clone, Copy)]
pub enum SchemaRole {
    Client,
    Server,
}

fn table(name: &str, columns: &[(&str, ColumnType)], unique: &[&str]) -> TableState {
    TableState {
        name: name.into(),
        columns: columns
            .iter()
            .map(|(name, ty)| ColumnState {
                field_name: (*name).into(),
                name: (*name).into(),
                column_type: *ty,
                nullable: false,
                primary_key: false,
                unique: false,
                default: None,
                check: None,
                choices: None,
                references: None,
                auto_now: false,
                auto_now_add: false,
            })
            .collect(),
        unique_constraints: vec![unique.iter().map(|name| (*name).into()).collect()],
        indexes: vec![],
    }
}

impl Registry {
    /// Application-owned compiled operations. Freeze these in an initial
    /// migration; generate reviewed follow-up operations when models evolve.
    /// Model tables themselves are supplied by the application's migrations.
    pub fn migration_operations(&self, role: SchemaRole) -> Vec<MigrationOperation> {
        use ColumnType::{Integer as I, Text as T};
        let tables = match role {
            SchemaRole::Client => {
                let mut tables = vec![
                    table("_sync_meta", &[("key", T), ("value", T)], &["key"]),
                    table(
                        "_sync_dirty",
                        &[("model", T), ("entity_id", T)],
                        &["model", "entity_id"],
                    ),
                    table(
                        "_sync_outbox",
                        &[("operation_id", T), ("payload", T)],
                        &["operation_id"],
                    ),
                    table(
                        "_sync_conflicts",
                        &[("model", T), ("entity_id", T), ("payload", T)],
                        &["model", "entity_id"],
                    ),
                ];
                for d in self.models.iter() {
                    let mut mirror = TableState::from(&d.schema);
                    mirror.name = d.mirror();
                    mirror.columns.push(ColumnState {
                        name: "_sync_epoch".into(),
                        field_name: "_sync_epoch".into(),
                        column_type: T,
                        nullable: true,
                        primary_key: false,
                        unique: false,
                        default: None,
                        check: None,
                        choices: None,
                        references: None,
                        auto_now: false,
                        auto_now_add: false,
                    });
                    mirror.indexes.clear();
                    mirror.unique_constraints.clear();
                    for column in &mut mirror.columns {
                        if !column.primary_key
                            && column.name != d.revision
                            && column.name != d.updated_at
                            && column.name != d.scope
                        {
                            column.nullable = true;
                        }
                        column.unique = false;
                        column.default = None;
                        column.check = None;
                        column.references = None;
                        column.auto_now = false;
                        column.auto_now_add = false;
                    }
                    tables.push(mirror);
                }
                tables
            }
            SchemaRole::Server => vec![
                table(
                    "_sync_effects",
                    &[("id", T), ("scope", T), ("principal", T), ("payload", T)],
                    &["id"],
                ),
                table(
                    "_sync_scopes",
                    &[("scope", T), ("epoch", T), ("sequence", I)],
                    &["scope"],
                ),
                table(
                    "_sync_changes",
                    &[("scope", T), ("sequence", I), ("payload", T)],
                    &["scope", "sequence"],
                ),
                table(
                    "_sync_receipts",
                    &[
                        ("scope", T),
                        ("replica_id", T),
                        ("operation_id", T),
                        ("request", T),
                        ("result", T),
                    ],
                    &["scope", "replica_id", "operation_id"],
                ),
            ],
        };
        tables
            .into_iter()
            .map(|table| {
                let identity = match role {
                    SchemaRole::Client => self
                        .models
                        .iter()
                        .find(|d| d.mirror() == table.name)
                        .map(|d| format!("shadow:{}", d.name))
                        .unwrap_or_else(|| table.name.clone()),
                    SchemaRole::Server => table.name.clone(),
                };
                MigrationOperation::CreateModel {
                    model: ModelState {
                        key: ModelKey {
                            app: "che_sync".into(),
                            name: identity,
                        },
                        table,
                    },
                }
            })
            .collect()
    }

    /// Include sync-owned tables in the desired state used by a migration
    /// planner, so they are not mistaken for removed application models.
    pub fn extend_migration_state(
        &self,
        state: &mut ProjectState,
        role: SchemaRole,
    ) -> Result<(), MigrationError> {
        for operation in self.migration_operations(role) {
            operation.state_forwards(state)?;
        }
        Ok(())
    }
}
