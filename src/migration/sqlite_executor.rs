//! SQLite forward migration execution and history recording.
use crate::{
    Migration, MigrationGraph, MigrationId, MigrationOperation, ProjectState, SqliteSchemaEditor,
    rusqlite::OptionalExtension,
};
use std::collections::{BTreeSet, HashMap};
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
        if let Err(error) = connection.execute_batch("BEGIN IMMEDIATE") {
            connection.pragma_update(None, "foreign_keys", foreign_keys)?;
            return Err(error.into());
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

fn assert_rebuild_objects_match_history(
    connection: &crate::rusqlite::Connection,
    table: &str,
    migration_state: &ProjectState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
    let expected_primary_key = expected
        .columns
        .iter()
        .filter(|column| column.primary_key)
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
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
                let position = expected
                    .indexes
                    .iter()
                    .enumerate()
                    .find(|(i, _)| name == format!("{table}_idx_{i}"));
                let Some((_, columns)) = position else {
                    return Err(format!(
                        "cannot rebuild {table}: unmanaged index {name}; use explicit RunSql"
                    )
                    .into());
                };
                if unique
                    || partial
                    || info
                        .0
                        .iter()
                        .any(|(_, desc, coll)| *desc || coll != "BINARY")
                    || info.1 != *columns
                {
                    return Err(format!("cannot rebuild {table}: divergent index {name}").into());
                }
            }
            "u" if unique && !partial && expected_unique.contains(&info.1) => {}
            "pk" if !partial && info.1 == expected_primary_key => {}
            _ => {
                return Err(format!(
                    "cannot rebuild {table}: unmanaged or divergent index {name}; use explicit RunSql"
                )
                .into());
            }
        }
    }
    let dependents = sqlite_opaque_dependents(connection, table)?;
    if !dependents.is_empty() {
        return Err(format!(
            "cannot rebuild table {table}; SQLite object(s) {} may depend on it. Use an explicit RunSql migration that recreates them and declares matching state operations",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColumnState, ColumnType, MigrationId, TableState};

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
