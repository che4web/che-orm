//! Backend-independent model registration for local-first applications.
use crate::{ColumnSchema, ColumnType, Model, TableSchema, Uuid};

/// Role of a synchronized model in a particular database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRole {
    Client,
    Server,
}

/// Stable protocol name mapped to a model-controlled SQL column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncField {
    pub name: &'static str,
    pub column: &'static str,
    pub writable: bool,
}

impl SyncField {
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

/// Shared model definition. Its client/server role belongs to registration.
pub trait SyncModel: Model<PrimaryKey = Uuid> + Send + Sync + 'static {
    const SYNC_NAME: &'static str;
    const SYNC_VERSION: u32 = 1;
    /// Derived sync models keep service columns out of the Rust struct.
    const AUTOMATIC_METADATA: bool = false;
    const REVISION_COLUMN: &'static str = "revision";
    const UPDATED_AT_COLUMN: &'static str = "updated_at";
    const DELETED_AT_COLUMN: &'static str = "deleted_at";
    const SCOPE_COLUMN: &'static str = "sync_scope";
    fn sync_fields() -> Vec<SyncField>;

    fn registered_fields() -> Vec<SyncField> {
        let mut fields = Self::sync_fields();
        for (name, column) in [
            ("revision", Self::REVISION_COLUMN),
            ("updated_at", Self::UPDATED_AT_COLUMN),
            ("deleted_at", Self::DELETED_AT_COLUMN),
            ("sync_scope", Self::SCOPE_COLUMN),
        ] {
            if !fields.iter().any(|field| field.column == column) {
                fields.push(SyncField::server_owned(name, column));
            }
        }
        fields
    }

    /// Physical schema, including infrastructure-owned service columns.
    fn sync_schema() -> TableSchema {
        let mut schema = Self::schema();
        if Self::AUTOMATIC_METADATA {
            for (name, ty, nullable, default) in [
                (Self::REVISION_COLUMN, ColumnType::Integer, false, Some("0")),
                (Self::UPDATED_AT_COLUMN, ColumnType::DateTime, false, None),
                (Self::DELETED_AT_COLUMN, ColumnType::DateTime, true, None),
                (Self::SCOPE_COLUMN, ColumnType::Text, false, Some("''")),
            ] {
                let mut column = ColumnSchema::new(name, ty, nullable);
                column.default = default;
                column.auto_now = name == Self::UPDATED_AT_COLUMN;
                schema.columns.push(column);
            }
        }
        schema
    }
}

/// Runtime-independent registration consumed by che-sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncRegistration {
    pub role: ModelRole,
    pub name: &'static str,
    pub version: u32,
    pub schema: TableSchema,
    pub fields: Vec<SyncField>,
    pub protected_columns: Vec<&'static str>,
    pub revision: &'static str,
    pub updated_at: &'static str,
    pub deleted_at: &'static str,
    pub scope: &'static str,
}

impl SyncRegistration {
    pub fn new<M: SyncModel>(role: ModelRole) -> Self {
        Self {
            role,
            name: M::SYNC_NAME,
            version: M::SYNC_VERSION,
            schema: M::sync_schema(),
            fields: M::registered_fields(),
            protected_columns: M::write_protected_columns().to_vec(),
            revision: M::REVISION_COLUMN,
            updated_at: M::UPDATED_AT_COLUMN,
            deleted_at: M::DELETED_AT_COLUMN,
            scope: M::SCOPE_COLUMN,
        }
    }
}

/// Infrastructure tables shared by all registrations of a database role.
pub fn sync_migration_operations(
    registrations: &[SyncRegistration],
) -> Result<Vec<crate::MigrationOperation>, crate::MigrationError> {
    let Some(first) = registrations.first() else {
        return Ok(vec![]);
    };
    sync_migration_operations_for_role(first.role, registrations)
}

/// Explicit role variant, also useful for an empty legacy registry.
pub fn sync_migration_operations_for_role(
    role: ModelRole,
    registrations: &[SyncRegistration],
) -> Result<Vec<crate::MigrationOperation>, crate::MigrationError> {
    use crate::{
        ColumnState, MigrationError, MigrationOperation, ModelKey, ModelState, TableState,
    };
    use ColumnType::{Integer as I, Text as T};
    if registrations
        .iter()
        .any(|registration| registration.role != role)
    {
        return Err(MigrationError::InvalidState(
            "a database cannot mix client and server sync roles".into(),
        ));
    }
    fn table(name: &str, columns: &[(&str, ColumnType)], unique: &[&str]) -> crate::TableState {
        crate::TableState {
            name: name.into(),
            columns: columns
                .iter()
                .map(|(name, ty)| crate::ColumnState {
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
    let mut tables = match role {
        ModelRole::Client => vec![
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
        ],
        ModelRole::Server => vec![
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
    if role == ModelRole::Client {
        for registration in registrations {
            let mut shadow = TableState::from(&registration.schema);
            shadow.name = format!("_sync_server_{}", registration.schema.name);
            shadow.indexes.clear();
            shadow.unique_constraints.clear();
            for column in &mut shadow.columns {
                if !column.primary_key
                    && ![
                        registration.revision,
                        registration.updated_at,
                        registration.scope,
                    ]
                    .contains(&column.name.as_str())
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
            shadow.columns.push(ColumnState {
                field_name: "_sync_epoch".into(),
                name: "_sync_epoch".into(),
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
            tables.push(shadow);
        }
    }
    Ok(tables
        .into_iter()
        .map(|table| {
            let identity = registrations
                .iter()
                .find(|registration| {
                    table.name == format!("_sync_server_{}", registration.schema.name)
                })
                .map(|registration| format!("shadow:{}", registration.name))
                .unwrap_or_else(|| table.name.clone());
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
        .collect())
}
