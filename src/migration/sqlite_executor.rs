//! SQLite forward migration execution and history recording.
use crate::{
    Migration, MigrationGraph, MigrationId, MigrationOperation, ProjectState, SqliteSchemaEditor,
    rusqlite::OptionalExtension,
};
use std::collections::{BTreeSet, HashMap};
use std::time::Duration;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub fn apply_operation_migrations_on_connection(
    connection: &mut crate::rusqlite::Connection,
    graph: MigrationGraph,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    apply_operation_migrations_to_on_connection(connection, graph, None)
}

/// Applies pending migrations needed to reach `target`, or every pending
/// migration when `target` is `None`. Target planning is forward-only.
pub fn apply_operation_migrations_to_on_connection(
    connection: &mut crate::rusqlite::Connection,
    graph: MigrationGraph,
    target: Option<MigrationId>,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    graph.validate_heads()?;
    // Validate static target errors before acquiring a write lock or changing
    // connection settings. The actual pending plan is recomputed under each
    // lock because another process may commit between migrations.
    graph.plan_forward(&BTreeSet::new(), target.as_ref())?;
    let mut applied_count = 0;
    loop {
        let foreign_keys: bool =
            connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        connection.pragma_update(None, "foreign_keys", false)?;
        if let Err(error) = begin_immediate(connection, Duration::from_secs(5)) {
            connection.pragma_update(None, "foreign_keys", foreign_keys)?;
            return Err(error);
        }
        let result = (|| {
            // Read history only after acquiring the write lock. Recompute this
            // per migration so concurrent manage processes cannot replay a
            // stale pending set after the previous commit.
            let applied = applied_operation_migrations(connection)?;
            let pending = pending_operation_migrations(&graph, &applied)?;
            let applied_ids = applied
                .keys()
                .map(|(app, name)| MigrationId {
                    app: app.clone(),
                    name: name.clone(),
                })
                .collect::<BTreeSet<_>>();
            let planned_ids = graph
                .plan_forward(&applied_ids, target.as_ref())?
                .into_iter()
                .map(|step| step.id)
                .collect::<BTreeSet<_>>();
            let Some(migration) = pending
                .into_iter()
                .find(|migration| planned_ids.contains(&migration.id))
            else {
                return Ok(None);
            };
            connection.execute_batch(OPERATION_MIGRATION_HISTORY_SQL)?;
            let mut migration_state = graph
                .replay_selected(ProjectState::default(), &applied_ids)
                .map_err(|error| format!("cannot reconstruct migration state: {error}"))?;
            apply_single_operation_migration(connection, migration, &mut migration_state)?;
            Ok(Some(()))
        })();
        let result = result.and_then(|applied| {
            connection.execute_batch("COMMIT")?;
            Ok(applied)
        });
        if !connection.is_autocommit() {
            connection.execute_batch("ROLLBACK")?;
        }
        connection.pragma_update(None, "foreign_keys", foreign_keys)?;
        match result {
            Ok(Some(())) => applied_count += 1,
            Ok(None) => return Ok(applied_count),
            Err(error) => return Err(error),
        }
    }
}

fn begin_immediate(
    connection: &mut crate::rusqlite::Connection,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    connection.busy_timeout(timeout)?;
    match connection.execute_batch("BEGIN IMMEDIATE") {
        Ok(()) => Ok(()),
        Err(crate::rusqlite::Error::SqliteFailure(error, _))
            if matches!(
                error.code,
                crate::rusqlite::ErrorCode::DatabaseBusy
                    | crate::rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            Err(format!(
                "database is locked; could not acquire the migration write lock within {} ms",
                timeout.as_millis()
            )
            .into())
        }
        Err(error) => Err(error.into()),
    }
}

fn apply_single_operation_migration(
    connection: &mut crate::rusqlite::Connection,
    migration: &Migration,
    migration_state: &mut ProjectState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for operation in &migration.operations {
        match operation {
            MigrationOperation::RunSql { forward, .. } => {
                use crate::rusqlite::hooks::{AuthAction, AuthContext, Authorization};
                connection.authorizer(Some(|context: AuthContext<'_>| match context.action {
                    AuthAction::Transaction { .. }
                    | AuthAction::Savepoint { .. }
                    | AuthAction::Attach { .. }
                    | AuthAction::Detach { .. } => Authorization::Deny,
                    _ => Authorization::Allow,
                }));
                let result = connection.execute_batch(forward);
                connection.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
                result?;
                operation.state_forwards(migration_state)?;
            }
            _ => {
                if let Some(table) = SqliteSchemaEditor::rebuild_table_name(operation) {
                    ensure_rebuild_can_preserve_rows(connection, operation)?;
                    assert_rebuild_objects_match_history(connection, table, migration_state)?;
                }
                let plan = SqliteSchemaEditor::plan(operation, migration_state)?;
                for statement in &plan.statements {
                    connection.execute_batch(statement)?;
                }
                *migration_state = plan.resulting_state;
            }
        }
    }
    connection.execute(
        "INSERT INTO che_migration_history (app, name, checksum, applied_on) VALUES (?1, ?2, ?3, ?4)",
        crate::rusqlite::params![
            migration.id.app,
            migration.id.name,
            migration.checksum,
            OffsetDateTime::now_utc().format(&Rfc3339)?,
        ],
    )?;
    assert_foreign_keys_valid(connection)?;
    Ok(())
}

fn ensure_rebuild_can_preserve_rows(
    connection: &crate::rusqlite::Connection,
    operation: &MigrationOperation,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match operation {
        MigrationOperation::AddColumn { table, column }
            if !column.nullable
                && !column.primary_key
                && column.default.is_none()
                && !column.auto_now_add
                && !column.auto_now =>
        {
            let table = quote_identifier(table);
            let has_rows: bool = connection.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table} LIMIT 1)"),
                [],
                |row| row.get(0),
            )?;
            if has_rows {
                return Err(format!(
                    "cannot add required column {} without a value for existing rows; add a default or use an explicit nullable → backfill → non-null migration",
                    column.name
                )
                .into());
            }
        }
        MigrationOperation::AlterColumn {
            table,
            old_column,
            new_column,
        } if old_column.nullable && !new_column.nullable => {
            let has_nulls: bool = connection.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {} WHERE {} IS NULL LIMIT 1)",
                    quote_identifier(table),
                    quote_identifier(&new_column.name)
                ),
                [],
                |row| row.get(0),
            )?;
            if has_nulls {
                return Err(format!(
                    "cannot make {}.{} non-null while existing rows contain NULL; backfill values before this migration",
                    table, new_column.name
                )
                .into());
            }
        }
        MigrationOperation::AddUniqueConstraint { table, columns } => {
            ensure_unique_values(connection, table, columns)?;
        }
        MigrationOperation::AlterColumn {
            table,
            old_column,
            new_column,
        } if !old_column.unique && new_column.unique => {
            ensure_unique_values(connection, table, std::slice::from_ref(&new_column.name))?;
        }
        _ => {}
    }
    Ok(())
}

fn ensure_unique_values(
    connection: &crate::rusqlite::Connection,
    table: &str,
    columns: &[String],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let columns_sql = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let non_null = columns
        .iter()
        .map(|column| format!("{} IS NOT NULL", quote_identifier(column)))
        .collect::<Vec<_>>()
        .join(" AND ");
    let duplicates: bool = connection.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM {} WHERE {non_null} GROUP BY {columns_sql} HAVING count(*) > 1 LIMIT 1)",
            quote_identifier(table)
        ),
        [],
        |row| row.get(0),
    )?;
    if duplicates {
        return Err(format!(
            "cannot add unique constraint on {}({}); existing rows contain duplicates",
            table,
            columns.join(", ")
        )
        .into());
    }
    Ok(())
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn assert_rebuild_objects_match_history(
    connection: &crate::rusqlite::Connection,
    table: &str,
    migration_state: &ProjectState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let table_sql: Option<String> = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .optional()?;
    if table_sql
        .as_deref()
        .is_some_and(|sql| sql.to_ascii_uppercase().contains("AUTOINCREMENT"))
    {
        return Err(format!(
            "cannot rebuild {table}: AUTOINCREMENT is not represented in historical state; use explicit RunSql that preserves sqlite_sequence"
        )
        .into());
    }
    // A rebuild drops every index owned by the old table. Refuse unknown
    // definitions instead of silently losing them.
    let expected = migration_state
        .tables()
        .iter()
        .find(|item| item.name == table)
        .ok_or_else(|| format!("missing historical table {table}"))?;
    let expected_unique = expected
        .columns
        .iter()
        .filter(|column| column.unique)
        .map(|column| vec![column.name.clone()])
        .chain(expected.unique_constraints.iter().cloned())
        .collect::<Vec<_>>();
    let mut unmatched_unique = expected_unique.clone();
    let expected_primary_key = expected
        .columns
        .iter()
        .filter(|column| column.primary_key)
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    let mut unmatched_indexes = expected.indexes.clone();
    let mut indexes = connection
        .prepare("SELECT name, \"unique\", origin, partial FROM pragma_index_list(?1)")?;
    let indexes = indexes
        .query_map([table], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (name, unique, origin, partial) in indexes {
        let info = sqlite_index_columns(connection, &name)?;
        match origin.as_str() {
            "c" => {
                if partial
                    || info
                        .0
                        .iter()
                        .any(|(_, desc, coll)| *desc || coll != "BINARY")
                {
                    return Err(format!("cannot rebuild {table}: divergent index {name}").into());
                }
                let unmatched = if unique {
                    &mut unmatched_unique
                } else {
                    &mut unmatched_indexes
                };
                let Some(position) = unmatched.iter().position(|columns| *columns == info.1) else {
                    return Err(format!(
                        "cannot rebuild {table}: unmanaged index {name}; use explicit RunSql"
                    )
                    .into());
                };
                unmatched.remove(position);
            }
            "u" if unique && !partial => {
                let Some(position) = unmatched_unique
                    .iter()
                    .position(|columns| *columns == info.1)
                else {
                    return Err(format!(
                        "cannot rebuild {table}: unmanaged or divergent index {name}; use explicit RunSql"
                    )
                    .into());
                };
                unmatched_unique.remove(position);
            }
            "pk" if !partial && info.1 == expected_primary_key => {}
            _ => {
                return Err(format!(
                    "cannot rebuild {table}: unmanaged or divergent index {name}; use explicit RunSql"
                )
                .into());
            }
        }
    }
    if !unmatched_indexes.is_empty() {
        return Err(format!(
            "cannot rebuild {table}: historical index(es) {:?} are missing from SQLite",
            unmatched_indexes
        )
        .into());
    }
    if !unmatched_unique.is_empty() {
        return Err(format!(
            "cannot rebuild {table}: historical unique constraint(s) {:?} are missing from SQLite",
            unmatched_unique
        )
        .into());
    }
    let dependents = sqlite_opaque_dependents(connection, table)?;
    if !dependents.is_empty() {
        return Err(format!(
            "cannot rebuild table {table}; SQLite object(s) {} may depend on it. Drop and recreate the matching declared schema objects around the table operation",
            dependents.join(", ")
        )
        .into());
    }
    Ok(())
}

fn sqlite_index_columns(
    connection: &crate::rusqlite::Connection,
    name: &str,
) -> Result<(Vec<(String, bool, String)>, Vec<String>), crate::rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT name, desc, coll FROM pragma_index_xinfo(?1) WHERE key = 1 ORDER BY seqno",
    )?;
    let details = statement
        .query_map([name], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<Vec<(String, bool, String)>, _>>()?;
    let columns = details.iter().map(|(name, _, _)| name.clone()).collect();
    Ok((details, columns))
}

fn sqlite_opaque_dependents(
    connection: &crate::rusqlite::Connection,
    table: &str,
) -> Result<Vec<String>, crate::rusqlite::Error> {
    let table = table.to_ascii_lowercase();
    let mut statement = connection.prepare(
        "SELECT type, name, tbl_name, sql FROM sqlite_master WHERE type IN ('trigger', 'view') AND sql IS NOT NULL ORDER BY type, name",
    )?;
    statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .filter_map(|row| match row {
            Ok((kind, name, owner, sql))
                if owner.eq_ignore_ascii_case(&table)
                    || sql.to_ascii_lowercase().contains(&table) =>
            {
                Some(Ok(format!("{kind} {name}")))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

pub fn pending_operation_migrations<'a>(
    graph: &'a MigrationGraph,
    applied: &HashMap<(String, String), String>,
) -> Result<Vec<&'a Migration>, Box<dyn std::error::Error + Send + Sync>> {
    graph.validate_heads()?;
    let registered = graph
        .ordered()
        .map(|migration| {
            (
                (migration.id.app.clone(), migration.id.name.clone()),
                migration,
            )
        })
        .collect::<HashMap<_, _>>();
    for ((app, name), checksum) in applied {
        match registered.get(&(app.clone(), name.clone())) {
            Some(migration) if migration.checksum == *checksum => {}
            Some(_) => return Err(format!("divergent operation migration {app}:{name}").into()),
            None => return Err(format!("missing operation migration {app}:{name}").into()),
        }
    }
    for migration in graph.ordered() {
        let key = (migration.id.app.clone(), migration.id.name.clone());
        if !applied.contains_key(&key) {
            continue;
        }
        for dependency in &migration.dependencies {
            let dependency_key = (dependency.app.clone(), dependency.name.clone());
            if !applied.contains_key(&dependency_key) {
                return Err(format!(
                    "inconsistent operation migration history: applied {}:{} is missing dependency {}:{}",
                    migration.id.app, migration.id.name, dependency.app, dependency.name
                )
                .into());
            }
        }
    }
    Ok(graph
        .ordered()
        .filter(|migration| {
            !applied.contains_key(&(migration.id.app.clone(), migration.id.name.clone()))
        })
        .collect())
}

const OPERATION_MIGRATION_HISTORY_SQL: &str = "CREATE TABLE IF NOT EXISTS che_migration_history (app TEXT NOT NULL, name TEXT NOT NULL, checksum TEXT NOT NULL, applied_on TEXT NOT NULL, PRIMARY KEY (app, name));";

pub fn applied_operation_migrations(
    connection: &crate::rusqlite::Connection,
) -> Result<HashMap<(String, String), String>, Box<dyn std::error::Error + Send + Sync>> {
    let has_history = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'che_migration_history'",
            [],
            |_| Ok(()),
        )
        .optional()?;
    if has_history.is_none() {
        return Ok(HashMap::new());
    }
    let mut statement =
        connection.prepare("SELECT app, name, checksum FROM che_migration_history")?;
    let rows = statement.query_map([], |row| {
        Ok((
            (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
            row.get(2)?,
        ))
    })?;
    rows.collect::<Result<HashMap<_, _>, _>>()
        .map_err(Into::into)
}

fn assert_foreign_keys_valid(
    connection: &crate::rusqlite::Connection,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut statement = connection
        .prepare("PRAGMA foreign_key_check;")
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?;
    let mut rows = statement
        .query([])
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?;
    if rows
        .next()
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?
        .is_some()
    {
        return Err("migration left foreign key violations".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ColumnState, ColumnType, ForeignKeyState, MigrationId, SchemaObjectKind, SchemaObjectState,
        TableState,
    };

    fn column(name: &str) -> ColumnState {
        ColumnState {
            field_name: name.into(),
            name: name.into(),
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
        }
    }

    fn state_with_table(columns: Vec<ColumnState>) -> ProjectState {
        let mut state = ProjectState::default();
        MigrationOperation::CreateTable {
            table: TableState {
                name: "items".into(),
                columns,
                unique_constraints: vec![],
                indexes: vec![],
            },
        }
        .state_forwards(&mut state)
        .unwrap();
        state
    }

    #[test]
    fn declared_trigger_can_be_dropped_rebuilt_and_restored_with_its_table() {
        let trigger = SchemaObjectState {
            app: "items".into(),
            kind: SchemaObjectKind::Trigger,
            name: "items_guard".into(),
            sql: "CREATE TRIGGER items_guard BEFORE INSERT ON items BEGIN SELECT 1; END".into(),
        };
        let initial = Migration::new(
            MigrationId {
                app: "items".into(),
                name: "0001_initial".into(),
            },
            vec![],
            vec![
                MigrationOperation::CreateTable {
                    table: TableState {
                        name: "items".into(),
                        columns: vec![column("id")],
                        unique_constraints: vec![],
                        indexes: vec![],
                    },
                },
                MigrationOperation::CreateSchemaObject {
                    object: trigger.clone(),
                },
            ],
        );
        let mut title = column("title");
        title.nullable = true;
        let rebuild = Migration::new(
            MigrationId {
                app: "items".into(),
                name: "0002_title".into(),
            },
            vec![initial.id.clone()],
            vec![
                MigrationOperation::DropSchemaObject {
                    kind: SchemaObjectKind::Trigger,
                    name: trigger.name.clone(),
                },
                MigrationOperation::AddColumn {
                    table: "items".into(),
                    column: title,
                },
                MigrationOperation::CreateSchemaObject { object: trigger },
            ],
        );
        let mut connection = crate::rusqlite::Connection::open_in_memory().unwrap();
        apply_operation_migrations_on_connection(
            &mut connection,
            MigrationGraph::new(vec![initial.clone()]).unwrap(),
        )
        .unwrap();
        connection
            .execute("INSERT INTO items(id) VALUES ('one')", [])
            .unwrap();
        apply_operation_migrations_on_connection(
            &mut connection,
            MigrationGraph::new(vec![initial, rebuild]).unwrap(),
        )
        .unwrap();
        let row: (String, Option<String>) = connection
            .query_row("SELECT id, title FROM items", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(row, ("one".into(), None));
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'trigger' AND name = 'items_guard')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists);
    }

    #[test]
    fn schema_object_operations_create_and_drop_sqlite_objects() {
        let create = Migration::new(
            MigrationId {
                app: "items".into(),
                name: "0001_create_view".into(),
            },
            vec![],
            vec![MigrationOperation::CreateSchemaObject {
                object: SchemaObjectState {
                    app: "items".into(),
                    kind: SchemaObjectKind::View,
                    name: "item_view".into(),
                    sql: "CREATE VIEW item_view AS SELECT 1 AS id".into(),
                },
            }],
        );
        let drop = Migration::new(
            MigrationId {
                app: "items".into(),
                name: "0002_drop_view".into(),
            },
            vec![create.id.clone()],
            vec![MigrationOperation::DropSchemaObject {
                kind: SchemaObjectKind::View,
                name: "item_view".into(),
            }],
        );
        let mut connection = crate::rusqlite::Connection::open_in_memory().unwrap();
        apply_operation_migrations_to_on_connection(
            &mut connection,
            MigrationGraph::new(vec![create.clone(), drop.clone()]).unwrap(),
            Some(create.id.clone()),
        )
        .unwrap();
        assert_eq!(
            connection
                .query_row("SELECT id FROM item_view", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        apply_operation_migrations_on_connection(
            &mut connection,
            MigrationGraph::new(vec![create, drop]).unwrap(),
        )
        .unwrap();
        let exists: Option<String> = connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'view' AND name = 'item_view'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert!(exists.is_none());
    }

    #[test]
    fn rebuild_rejects_unmanaged_auto_unique_indexes() {
        let connection = crate::rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE items (id TEXT NOT NULL, obsolete TEXT NOT NULL, UNIQUE(obsolete))",
            )
            .unwrap();
        let error = assert_rebuild_objects_match_history(
            &connection,
            "items",
            &state_with_table(vec![column("id"), column("obsolete")]),
        )
        .unwrap_err();
        assert!(error.to_string().contains("unmanaged or divergent index"));
    }

    #[test]
    fn rebuild_accepts_historical_unique_constraints() {
        let connection = crate::rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE items (id TEXT NOT NULL, UNIQUE(id))")
            .unwrap();
        let mut id = column("id");
        id.unique = true;
        assert!(assert_rebuild_objects_match_history(
            &connection,
            "items",
            &state_with_table(vec![id]),
        )
        .is_ok());
    }

    #[test]
    fn consecutive_index_changes_preserve_rows_and_match_history() {
        for add_after_removal in [false, true] {
            let table = TableState {
                name: "items".into(),
                columns: vec![column("a"), column("b"), column("c")],
                unique_constraints: vec![],
                indexes: vec![vec!["a".into()], vec!["b".into()], vec!["c".into()]],
            };
            let initial = Migration::new(
                MigrationId {
                    app: "items".into(),
                    name: "0001".into(),
                },
                vec![],
                vec![MigrationOperation::CreateTable { table }],
            );
            let mut db = crate::rusqlite::Connection::open_in_memory().unwrap();
            apply_operation_migrations_on_connection(
                &mut db,
                MigrationGraph::new(vec![initial.clone()]).unwrap(),
            )
            .unwrap();
            db.execute("INSERT INTO items VALUES ('A','B','C')", [])
                .unwrap();
            // Historical SQL can use names different from the generated ones.
            db.execute_batch("DROP INDEX items_idx_0; CREATE INDEX legacy_a ON items(a)")
                .unwrap();
            let last = if add_after_removal {
                MigrationOperation::AddIndex {
                    table: "items".into(),
                    columns: vec!["a".into(), "b".into()],
                }
            } else {
                MigrationOperation::RemoveIndex {
                    table: "items".into(),
                    columns: vec!["c".into()],
                }
            };
            let next = Migration::new(
                MigrationId {
                    app: "items".into(),
                    name: "0002".into(),
                },
                vec![initial.id.clone()],
                vec![
                    MigrationOperation::RemoveIndex {
                        table: "items".into(),
                        columns: vec!["a".into()],
                    },
                    last,
                ],
            );
            let graph = MigrationGraph::new(vec![initial, next]).unwrap();
            apply_operation_migrations_on_connection(&mut db, graph.clone()).unwrap();
            assert_rebuild_objects_match_history(
                &db,
                "items",
                &graph.replay(Default::default()).unwrap(),
            )
            .unwrap();
            let row: (String, String, String) = db
                .query_row("SELECT a,b,c FROM items", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .unwrap();
            assert_eq!(row, ("A".into(), "B".into(), "C".into()));
            assert_eq!(
                apply_operation_migrations_on_connection(&mut db, graph).unwrap(),
                0
            );
        }
    }

    #[test]
    fn rebuild_accepts_historical_non_unique_index_names() {
        let connection = crate::rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE items (id TEXT NOT NULL); \
                 CREATE INDEX legacy_items_id_idx ON items(id)",
            )
            .unwrap();
        let mut state = ProjectState::default();
        MigrationOperation::CreateTable {
            table: TableState {
                name: "items".into(),
                columns: vec![column("id")],
                unique_constraints: vec![],
                indexes: vec![vec!["id".into()]],
            },
        }
        .state_forwards(&mut state)
        .unwrap();
        assert!(assert_rebuild_objects_match_history(&connection, "items", &state).is_ok());
    }

    #[test]
    fn rebuild_rejects_views_and_external_triggers_that_reference_the_table() {
        let connection = crate::rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE items (id TEXT NOT NULL); \
                 CREATE TABLE audit (item_id TEXT); \
                 CREATE VIEW item_view AS SELECT id FROM items; \
                 CREATE TRIGGER audit_items AFTER INSERT ON audit BEGIN SELECT id FROM items; END;",
            )
            .unwrap();
        let error = assert_rebuild_objects_match_history(
            &connection,
            "items",
            &state_with_table(vec![column("id")]),
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("view item_view"));
        assert!(message.contains("trigger audit_items"));
    }

    #[test]
    fn rebuild_rejects_required_column_without_a_value_for_existing_rows() {
        let connection = crate::rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE items (id TEXT NOT NULL); INSERT INTO items VALUES ('1')")
            .unwrap();
        let mut required = column("title");
        required.nullable = false;
        let error = ensure_rebuild_can_preserve_rows(
            &connection,
            &MigrationOperation::AddColumn {
                table: "items".into(),
                column: required,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("nullable → backfill → non-null"));
    }

    #[test]
    fn rebuild_rejects_nulls_and_duplicates_before_new_constraints() {
        let connection = crate::rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE items (id TEXT, email TEXT); INSERT INTO items VALUES (NULL, 'same'), (NULL, 'same')")
            .unwrap();
        let mut old_id = column("id");
        old_id.nullable = true;
        let error = ensure_rebuild_can_preserve_rows(
            &connection,
            &MigrationOperation::AlterColumn {
                table: "items".into(),
                old_column: old_id.clone(),
                new_column: column("id"),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("contain NULL"));
        let error = ensure_rebuild_can_preserve_rows(
            &connection,
            &MigrationOperation::AddUniqueConstraint {
                table: "items".into(),
                columns: vec!["email".into()],
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("contain duplicates"));
    }

    #[test]
    fn backfill_allows_nullable_to_non_null_without_a_schema_default() {
        let mut title = column("title");
        title.nullable = true;
        let initial = Migration::new(
            MigrationId {
                app: "items".into(),
                name: "0001".into(),
            },
            vec![],
            vec![MigrationOperation::CreateTable {
                table: TableState {
                    name: "items".into(),
                    columns: vec![column("id"), title.clone()],
                    unique_constraints: vec![],
                    indexes: vec![],
                },
            }],
        );
        let mut required = title.clone();
        required.nullable = false;
        let follow_up = Migration::new(
            MigrationId {
                app: "items".into(),
                name: "0002_title_required".into(),
            },
            vec![initial.id.clone()],
            vec![
                MigrationOperation::BackfillColumn {
                    table: "items".into(),
                    column: "title".into(),
                    value: "'untitled'".into(),
                },
                MigrationOperation::AlterColumn {
                    table: "items".into(),
                    old_column: title,
                    new_column: required,
                },
            ],
        );
        let graph = MigrationGraph::new(vec![initial, follow_up]).unwrap();
        let mut connection = crate::rusqlite::Connection::open_in_memory().unwrap();
        apply_operation_migrations_on_connection(
            &mut connection,
            MigrationGraph::new(vec![graph.ordered().next().unwrap().clone()]).unwrap(),
        )
        .unwrap();
        connection
            .execute("INSERT INTO items(id, title) VALUES ('1', NULL)", [])
            .unwrap();
        assert_eq!(
            apply_operation_migrations_on_connection(&mut connection, graph).unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT title FROM items WHERE id='1'", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "untitled"
        );
        assert!(
            connection
                .execute("INSERT INTO items(id, title) VALUES ('2', NULL)", [])
                .is_err()
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT dflt_value FROM pragma_table_info('items') WHERE name='title'",
                    [],
                    |row| row.get::<_, Option<String>>(0)
                )
                .unwrap(),
            None
        );
    }

    #[test]
    fn rebuild_keeps_integer_primary_key_allocation_and_rejects_unmodeled_autoincrement() {
        let mut id = column("id");
        id.column_type = ColumnType::Integer;
        id.primary_key = true;
        let title = column("title");
        let initial = Migration::new(
            MigrationId {
                app: "items".into(),
                name: "0001".into(),
            },
            vec![],
            vec![MigrationOperation::CreateTable {
                table: TableState {
                    name: "items".into(),
                    columns: vec![id.clone(), title.clone()],
                    unique_constraints: vec![],
                    indexes: vec![],
                },
            }],
        );
        let mut new_title = title.clone();
        new_title.default = Some("'untitled'".into());
        let rebuild = Migration::new(
            MigrationId {
                app: "items".into(),
                name: "0002_title_default".into(),
            },
            vec![initial.id.clone()],
            vec![MigrationOperation::AlterColumn {
                table: "items".into(),
                old_column: title,
                new_column: new_title,
            }],
        );
        let mut connection = crate::rusqlite::Connection::open_in_memory().unwrap();
        apply_operation_migrations_on_connection(
            &mut connection,
            MigrationGraph::new(vec![initial.clone()]).unwrap(),
        )
        .unwrap();
        connection
            .execute("INSERT INTO items(id, title) VALUES (5, 'existing')", [])
            .unwrap();
        apply_operation_migrations_on_connection(
            &mut connection,
            MigrationGraph::new(vec![initial, rebuild]).unwrap(),
        )
        .unwrap();
        connection
            .execute("INSERT INTO items(title) VALUES ('next')", [])
            .unwrap();
        assert_eq!(
            connection
                .query_row("SELECT id FROM items WHERE title = 'next'", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            6
        );

        connection
            .execute_batch(
                "CREATE TABLE sequence_items(id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT)",
            )
            .unwrap();
        let error = assert_rebuild_objects_match_history(
            &connection,
            "sequence_items",
            &state_with_table(vec![id, column("title")]),
        )
        .unwrap_err();
        assert!(error.to_string().contains("AUTOINCREMENT"));
    }

    #[test]
    fn rebuild_preserves_composite_constraints_indexes_and_foreign_keys() {
        let mut id = column("id");
        id.column_type = ColumnType::Integer;
        id.primary_key = true;
        let mut parent_id = id.clone();
        parent_id.name = "parent_id".into();
        parent_id.field_name = "parent_id".into();
        parent_id.primary_key = false;
        parent_id.references = Some(ForeignKeyState {
            target: "parents(id)".into(),
            on_delete: Some("cascade".into()),
        });
        let code = column("code");
        let description = column("description");
        let initial = Migration::new(
            MigrationId {
                app: "items".into(),
                name: "0001".into(),
            },
            vec![],
            vec![
                MigrationOperation::CreateTable {
                    table: TableState {
                        name: "parents".into(),
                        columns: vec![id.clone()],
                        unique_constraints: vec![],
                        indexes: vec![],
                    },
                },
                MigrationOperation::CreateTable {
                    table: TableState {
                        name: "items".into(),
                        columns: vec![id.clone(), parent_id, code.clone(), description.clone()],
                        unique_constraints: vec![vec!["parent_id".into(), "code".into()]],
                        indexes: vec![vec!["code".into(), "description".into()]],
                    },
                },
            ],
        );
        let mut new_description = description.clone();
        new_description.default = Some("'unknown'".into());
        let rebuild = Migration::new(
            MigrationId {
                app: "items".into(),
                name: "0002_description_default".into(),
            },
            vec![initial.id.clone()],
            vec![MigrationOperation::AlterColumn {
                table: "items".into(),
                old_column: description,
                new_column: new_description,
            }],
        );
        let mut connection = crate::rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        apply_operation_migrations_on_connection(
            &mut connection,
            MigrationGraph::new(vec![initial.clone()]).unwrap(),
        )
        .unwrap();
        connection
            .execute("INSERT INTO parents(id) VALUES (1)", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO items(id, parent_id, code, description) VALUES (1, 1, 'A', 'first')",
                [],
            )
            .unwrap();
        let graph = MigrationGraph::new(vec![initial, rebuild]).unwrap();
        apply_operation_migrations_on_connection(&mut connection, graph.clone()).unwrap();
        assert_rebuild_objects_match_history(
            &connection,
            "items",
            &graph.replay(Default::default()).unwrap(),
        )
        .unwrap();
        assert!(connection
            .execute(
                "INSERT INTO items(id, parent_id, code, description) VALUES (2, 1, 'A', 'duplicate')",
                [],
            )
            .is_err());
        assert!(connection
            .execute(
                "INSERT INTO items(id, parent_id, code, description) VALUES (3, 99, 'B', 'missing parent')",
                [],
            )
            .is_err());
        assert_eq!(
            connection
                .query_row("SELECT description FROM items WHERE id=1", [], |row| row
                    .get::<_, String>(
                    0
                ))
                .unwrap(),
            "first"
        );
    }

    #[test]
    fn cyclic_foreign_key_tables_apply_with_their_declared_metadata() {
        let mut id = column("id");
        id.column_type = ColumnType::Integer;
        id.primary_key = true;
        let reference = |name: &str, target: &str| ColumnState {
            field_name: name.into(),
            name: name.into(),
            column_type: ColumnType::Integer,
            nullable: true,
            primary_key: false,
            unique: false,
            default: None,
            check: None,
            choices: None,
            references: Some(ForeignKeyState {
                target: target.into(),
                on_delete: Some("set null".into()),
            }),
            auto_now_add: false,
            auto_now: false,
        };
        let migration = Migration::new(
            MigrationId {
                app: "cycle".into(),
                name: "0001".into(),
            },
            vec![],
            vec![
                MigrationOperation::CreateTable {
                    table: TableState {
                        name: "left_items".into(),
                        columns: vec![id.clone(), reference("right_id", "right_items(id)")],
                        unique_constraints: vec![],
                        indexes: vec![],
                    },
                },
                MigrationOperation::CreateTable {
                    table: TableState {
                        name: "right_items".into(),
                        columns: vec![id, reference("left_id", "left_items(id)")],
                        unique_constraints: vec![],
                        indexes: vec![],
                    },
                },
            ],
        );
        let mut connection = crate::rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        apply_operation_migrations_on_connection(
            &mut connection,
            MigrationGraph::new(vec![migration]).unwrap(),
        )
        .unwrap();
        let left_target: String = connection
            .query_row(
                "SELECT \"table\" FROM pragma_foreign_key_list('left_items')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let right_target: String = connection
            .query_row(
                "SELECT \"table\" FROM pragma_foreign_key_list('right_items')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            (left_target, right_target),
            ("right_items".into(), "left_items".into())
        );
    }

    #[test]
    fn busy_write_lock_has_a_bounded_diagnostic_error() {
        let path = std::env::temp_dir().join(format!(
            "che_orm_lock_{}.sqlite",
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let first = crate::rusqlite::Connection::open(&path).unwrap();
        first.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let mut second = crate::rusqlite::Connection::open(&path).unwrap();
        let error = begin_immediate(&mut second, Duration::from_millis(10)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("migration write lock within 10 ms")
        );
        first.execute_batch("ROLLBACK").unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn transaction_commands_cannot_commit_partial_migrations() {
        for sql in [
            "CREATE TABLE partial(id); COMMIT;",
            "CREATE TABLE partial(id); ROLLBACK;",
            "CREATE TABLE partial(id); SAVEPOINT sneaky;",
        ] {
            let mut connection = crate::rusqlite::Connection::open_in_memory().unwrap();
            connection
                .pragma_update(None, "foreign_keys", true)
                .unwrap();
            let graph = MigrationGraph::new(vec![Migration::new(
                MigrationId {
                    app: "test".into(),
                    name: "0001".into(),
                },
                vec![],
                vec![MigrationOperation::RunSql {
                    forward: sql.into(),
                    reverse: None,
                    state_operations: vec![],
                }],
            )])
            .unwrap();
            assert!(apply_operation_migrations_on_connection(&mut connection, graph).is_err());
            assert!(connection.is_autocommit());
            assert!(
                connection
                    .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, bool>(0))
                    .unwrap()
            );
            assert_eq!(connection.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('partial', 'che_migration_history')", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        }
    }

    #[test]
    fn trigger_begin_is_not_transaction_control_and_fk_setting_is_restored() {
        let mut connection = crate::rusqlite::Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        let graph = MigrationGraph::new(vec![Migration::new(
            MigrationId { app: "test".into(), name: "0001".into() }, vec![],
            vec![MigrationOperation::RunSql { forward: "CREATE TABLE items(id); CREATE TRIGGER check_item BEFORE INSERT ON items BEGIN SELECT RAISE(ABORT, 'blocked'); END;".into(), reverse: None, state_operations: vec![] }],
        )]).unwrap();
        assert_eq!(
            apply_operation_migrations_on_connection(&mut connection, graph.clone()).unwrap(),
            1
        );
        assert_eq!(
            apply_operation_migrations_on_connection(&mut connection, graph).unwrap(),
            0
        );
        assert!(
            !connection
                .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, bool>(0))
                .unwrap()
        );
        assert!(
            connection
                .execute("INSERT INTO items VALUES(1)", [])
                .is_err()
        );
    }

    #[test]
    fn target_applies_only_its_forward_closure_and_rejects_rollback() {
        let initial = Migration::new(
            MigrationId {
                app: "tasks".into(),
                name: "0001".into(),
            },
            vec![],
            vec![MigrationOperation::RunSql {
                forward: "CREATE TABLE task_initial(id)".into(),
                reverse: None,
                state_operations: vec![],
            }],
        );
        let target = Migration::new(
            MigrationId {
                app: "tasks".into(),
                name: "0002".into(),
            },
            vec![initial.id.clone()],
            vec![MigrationOperation::RunSql {
                forward: "CREATE TABLE task_target(id)".into(),
                reverse: None,
                state_operations: vec![],
            }],
        );
        let independent = Migration::new(
            MigrationId {
                app: "audit".into(),
                name: "0001".into(),
            },
            vec![],
            vec![MigrationOperation::RunSql {
                forward: "CREATE TABLE audit_initial(id)".into(),
                reverse: None,
                state_operations: vec![],
            }],
        );
        let graph =
            MigrationGraph::new(vec![initial.clone(), target.clone(), independent]).unwrap();
        let mut connection = crate::rusqlite::Connection::open_in_memory().unwrap();
        assert_eq!(
            apply_operation_migrations_to_on_connection(
                &mut connection,
                graph.clone(),
                Some(target.id.clone()),
            )
            .unwrap(),
            2
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = 'audit_initial'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        let count: i64 = connection
            .query_row("SELECT count(*) FROM che_migration_history", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 2);
        assert!(
            apply_operation_migrations_to_on_connection(&mut connection, graph, Some(initial.id),)
                .unwrap_err()
                .to_string()
                .contains("rollback is not supported")
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM che_migration_history", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            2
        );
    }

    #[test]
    fn failed_migration_rolls_back_only_its_own_transaction() {
        let first = Migration::new(
            MigrationId {
                app: "tasks".into(),
                name: "0001".into(),
            },
            vec![],
            vec![MigrationOperation::RunSql {
                forward: "CREATE TABLE first_migration(id)".into(),
                reverse: None,
                state_operations: vec![],
            }],
        );
        let second = Migration::new(
            MigrationId {
                app: "tasks".into(),
                name: "0002".into(),
            },
            vec![first.id.clone()],
            vec![MigrationOperation::RunSql {
                forward: "CREATE TABLE failed_migration(id); INSERT INTO absent_table VALUES (1)"
                    .into(),
                reverse: None,
                state_operations: vec![],
            }],
        );
        let graph = MigrationGraph::new(vec![first.clone(), second]).unwrap();
        let mut connection = crate::rusqlite::Connection::open_in_memory().unwrap();
        assert!(apply_operation_migrations_on_connection(&mut connection, graph).is_err());
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = 'first_migration'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = 'failed_migration'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        let history = applied_operation_migrations(&connection).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(
            history.get(&(first.id.app, first.id.name)),
            Some(&first.checksum)
        );
    }
}
