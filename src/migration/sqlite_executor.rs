//! SQLite forward migration execution and history recording.
use crate::{
    Migration, MigrationGraph, MigrationOperation, ProjectState, SqliteSchemaEditor,
    rusqlite::OptionalExtension,
};
use std::collections::HashMap;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub fn apply_operation_migrations_on_connection(
    connection: &mut crate::rusqlite::Connection,
    graph: MigrationGraph,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    graph.validate_heads()?;
    let foreign_keys: bool = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    connection.pragma_update(None, "foreign_keys", false)?;
    if let Err(error) = connection.execute_batch("BEGIN IMMEDIATE") {
        connection.pragma_update(None, "foreign_keys", foreign_keys)?;
        return Err(error.into());
    }
    let result = (|| {
        // Read the migration history only after acquiring SQLite's write lock.
        // Otherwise concurrent manage processes can compute the same pending plan.
        let applied = applied_operation_migrations(connection)?;
        let pending = pending_operation_migrations(&graph, &applied)?;
        if pending.is_empty() {
            return Ok(0);
        }

        connection.execute_batch(OPERATION_MIGRATION_HISTORY_SQL)?;
        let applied_ids = applied
            .keys()
            .map(|(app, name)| crate::MigrationId {
                app: app.clone(),
                name: name.clone(),
            })
            .collect();
        let mut migration_state = graph
            .replay_selected(ProjectState::default(), &applied_ids)
            .map_err(|error| format!("cannot reconstruct migration state: {error}"))?;
        graph.plan_forward(&applied_ids, None)?;
        for migration in &pending {
            for operation in &migration.operations {
                match operation {
                    MigrationOperation::RunSql { forward, .. } => {
                        use crate::rusqlite::hooks::{AuthAction, AuthContext, Authorization};
                        connection.authorizer(Some(|context: AuthContext<'_>| {
                            match context.action {
                                AuthAction::Transaction { .. }
                                | AuthAction::Savepoint { .. }
                                | AuthAction::Attach { .. }
                                | AuthAction::Detach { .. } => Authorization::Deny,
                                _ => Authorization::Allow,
                            }
                        }));
                        let result = connection.execute_batch(forward);
                        connection.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
                        result?;
                        operation.state_forwards(&mut migration_state)?;
                    }
                    _ => {
                        if let Some(table) = SqliteSchemaEditor::rebuild_table_name(operation) {
                            // A rebuild drops every index owned by the old table.
                            // Refuse unknown definitions instead of silently losing them.
                            let expected = migration_state
                                .tables()
                                .iter()
                                .find(|item| item.name == table)
                                .ok_or_else(|| format!("missing historical table {table}"))?;
                            let mut indexes = connection.prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = ?1 AND sql IS NOT NULL")?;
                            let names = indexes
                                .query_map([table], |row| row.get::<_, String>(0))?
                                .collect::<Result<Vec<_>, _>>()?;
                            for name in names {
                                let position = expected
                                    .indexes
                                    .iter()
                                    .enumerate()
                                    .find(|(i, _)| name == format!("{table}_idx_{i}"));
                                let Some((_, columns)) = position else {
                                    return Err(format!("cannot rebuild {table}: unmanaged index {name}; use explicit RunSql").into());
                                };
                                let info: Vec<(String, bool, String)> = {
                                    let mut query = connection.prepare("SELECT name, desc, coll FROM pragma_index_xinfo(?1) WHERE key = 1 ORDER BY seqno")?;
                                    query
                                        .query_map([&name], |row| {
                                            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                                        })?
                                        .collect::<Result<_, _>>()?
                                };
                                let (unique, partial): (bool, bool) = connection.query_row("SELECT \"unique\", partial FROM pragma_index_list(?1) WHERE name = ?2", [table, &name], |row| Ok((row.get(0)?, row.get(1)?)))?;
                                if unique
                                    || partial
                                    || info.iter().any(|(_, desc, coll)| *desc || coll != "BINARY")
                                    || info.iter().map(|(name, _, _)| name).collect::<Vec<_>>()
                                        != columns.iter().collect::<Vec<_>>()
                                {
                                    return Err(format!(
                                        "cannot rebuild {table}: divergent index {name}"
                                    )
                                    .into());
                                }
                            }
                            let triggers = sqlite_table_triggers(connection, table)?;
                            if !triggers.is_empty() {
                                return Err(format!(
                                    "cannot rebuild table {table}; it owns SQLite trigger(s) {}. Use an explicit RunSql migration that recreates them and declares matching state operations",
                                    triggers.join(", ")
                                )
                                .into());
                            }
                        }
                        let plan = SqliteSchemaEditor::plan(operation, &migration_state)?;
                        for statement in &plan.statements {
                            connection.execute_batch(statement)?;
                        }
                        migration_state = plan.resulting_state;
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
        }
        assert_foreign_keys_valid(connection)?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(pending.len())
    })();

    let result = result.and_then(|count| {
        connection.execute_batch("COMMIT")?;
        Ok(count)
    });
    if !connection.is_autocommit() {
        connection.execute_batch("ROLLBACK")?;
    }
    connection.pragma_update(None, "foreign_keys", foreign_keys)?;
    result
}

fn sqlite_table_triggers(
    connection: &crate::rusqlite::Connection,
    table: &str,
) -> Result<Vec<String>, crate::rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'trigger' AND tbl_name = ?1 ORDER BY name",
    )?;
    statement
        .query_map([table], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()
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
    use crate::MigrationId;

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
