use crate::{
    CompiledQuery, DatabaseValue, DeferredForeignKey, MigrationGraph, MigrationId,
    MigrationOperation, OrmError, PostgresSchemaEditor, ProjectState, backend::Session,
    connection::TransactionGuard,
};
use std::collections::{BTreeSet, HashMap};

const HISTORY: &str = "CREATE TABLE IF NOT EXISTS che_migration_history (app TEXT NOT NULL, name TEXT NOT NULL, checksum TEXT NOT NULL, applied_on TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY (app, name))";

pub(crate) async fn history(
    session: &Session,
) -> Result<HashMap<(String, String), String>, OrmError> {
    let exists = session
        .query(CompiledQuery {
            sql: "SELECT to_regclass('che_migration_history') IS NOT NULL".into(),
            params: vec![],
        })
        .await?;
    if !exists[0].get::<bool>(0)? {
        return Ok(HashMap::new());
    }
    session
        .query(CompiledQuery {
            sql: "SELECT app, name, checksum FROM che_migration_history".into(),
            params: vec![],
        })
        .await?
        .iter()
        .map(|row| Ok(((row.get(0)?, row.get(1)?), row.get(2)?)))
        .collect()
}

pub(crate) async fn apply(
    session: &Session,
    graph: MigrationGraph,
    target: Option<MigrationId>,
) -> Result<usize, OrmError> {
    graph
        .validate_heads()
        .map_err(|error| OrmError::Migration(error.to_string()))?;
    graph
        .plan_forward(&BTreeSet::new(), target.as_ref())
        .map_err(|error| OrmError::Migration(error.to_string()))?;
    let mut count = 0;
    loop {
        let mut guard = TransactionGuard {
            session: session.clone(),
            active: true,
        };
        session.batch("BEGIN").await?;
        let result = async {
            session
                .batch("SELECT pg_advisory_xact_lock(716286318705851)")
                .await?;
            let applied = history(session).await?;
            validate_history(&graph, &applied)?;
            let applied_ids = applied
                .keys()
                .map(|(app, name)| MigrationId {
                    app: app.clone(),
                    name: name.clone(),
                })
                .collect::<BTreeSet<_>>();
            let pending = graph
                .plan_forward(&applied_ids, target.as_ref())
                .map_err(|error| OrmError::Migration(error.to_string()))?;
            let Some(step) = pending.first() else {
                return Ok(false);
            };
            let migration = graph
                .ordered()
                .find(|migration| migration.id == step.id)
                .unwrap();
            let mut state = graph
                .replay_selected(ProjectState::default(), &applied_ids)
                .map_err(|error| OrmError::Migration(error.to_string()))?;
            session.batch(HISTORY).await?;
            let mut deferred = Vec::new();
            for operation in &migration.operations {
                let plan = PostgresSchemaEditor::plan_with_deferred(operation, &state, &deferred)
                    .map_err(|error| OrmError::Migration(error.to_string()))?;
                for sql in &plan.statements {
                    session
                        .batch("SET LOCAL standard_conforming_strings = on")
                        .await?;
                    reject_transaction_control(sql)?;
                    session.batch(sql).await?;
                }
                if let MigrationOperation::AlterColumn {
                    table, old_column, ..
                } = operation
                {
                    deferred.retain(|key: &DeferredForeignKey| {
                        key.table != *table || key.column != old_column.name
                    });
                }
                for key in &mut deferred {
                    key.follow(operation);
                }
                state = plan.resulting_state;
                deferred.retain(|key| key.exists(&state));
                deferred.extend(plan.deferred_foreign_keys);
                apply_deferred(session, &state, &mut deferred, false).await?;
            }
            apply_deferred(session, &state, &mut deferred, true).await?;
            session
                .execute(CompiledQuery {
                    sql: "INSERT INTO che_migration_history (app,name,checksum) VALUES ($1,$2,$3)"
                        .into(),
                    params: vec![
                        DatabaseValue::Text(migration.id.app.clone()),
                        DatabaseValue::Text(migration.id.name.clone()),
                        DatabaseValue::Text(migration.checksum.clone()),
                    ],
                })
                .await?;
            Ok::<_, OrmError>(true)
        }
        .await;
        match result {
            Ok(applied) => {
                session.commit().await?;
                guard.active = false;
                if !applied {
                    return Ok(count);
                }
                count += 1;
            }
            Err(error) => {
                session.batch("ROLLBACK").await?;
                guard.active = false;
                return Err(error);
            }
        }
    }
}

async fn apply_deferred(
    session: &Session,
    state: &ProjectState,
    pending: &mut Vec<DeferredForeignKey>,
    finish: bool,
) -> Result<(), OrmError> {
    let mut waiting = Vec::new();
    for key in std::mem::take(pending) {
        if finish || key.ready(state) {
            let sql = key
                .statement(state)
                .map_err(|error| OrmError::Migration(error.to_string()))?;
            session.batch(&sql).await?;
        } else {
            waiting.push(key);
        }
    }
    *pending = waiting;
    Ok(())
}

fn validate_history(
    graph: &MigrationGraph,
    applied: &HashMap<(String, String), String>,
) -> Result<(), OrmError> {
    let migrations = graph
        .ordered()
        .map(|migration| {
            (
                (migration.id.app.clone(), migration.id.name.clone()),
                migration,
            )
        })
        .collect::<HashMap<_, _>>();
    for (key, checksum) in applied {
        let migration = migrations
            .get(key)
            .ok_or_else(|| OrmError::Migration(format!("missing migration {}:{}", key.0, key.1)))?;
        if migration.checksum != *checksum {
            return Err(OrmError::Migration(format!(
                "divergent migration {}:{}",
                key.0, key.1
            )));
        }
        for dep in &migration.dependencies {
            if !applied.contains_key(&(dep.app.clone(), dep.name.clone())) {
                return Err(OrmError::Migration("inconsistent migration history".into()));
            }
        }
    }
    Ok(())
}

// Scan statement-leading tokens, skipping PostgreSQL quotes, dollar bodies and
// comments. Migration SQL must not commit or escape the executor's transaction.
fn reject_transaction_control(sql: &str) -> Result<(), OrmError> {
    let bytes = sql.as_bytes();
    let mut i = 0;
    let mut first = true;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if bytes[i..].starts_with(b"--") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        if matches!(bytes[i], b'\'' | b'"') {
            let quote = bytes[i];
            let escapes = quote == b'\''
                && i > 0
                && matches!(bytes[i - 1], b'E' | b'e')
                && (i == 1 || !(bytes[i - 2].is_ascii_alphanumeric() || bytes[i - 2] == b'_'));
            i += 1;
            while i < bytes.len() {
                if escapes && bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else if bytes[i] == quote {
                    i += 1;
                    if i < bytes.len() && bytes[i] == quote {
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            first = false;
            continue;
        }
        if bytes[i] == b'$' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'$' {
                i += 1;
                let tag = &sql[start..i];
                if let Some(end) = sql[i..].find(tag) {
                    i += end + tag.len();
                } else {
                    i = bytes.len();
                }
                first = false;
                continue;
            }
            i = start + 1;
        }
        if bytes[i] == b';' {
            first = true;
            i += 1;
            continue;
        }
        if bytes[i].is_ascii_alphabetic() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let token = sql[start..i].to_ascii_uppercase();
            if first
                && matches!(
                    token.as_str(),
                    "BEGIN"
                        | "START"
                        | "COMMIT"
                        | "END"
                        | "ROLLBACK"
                        | "ABORT"
                        | "SAVEPOINT"
                        | "RELEASE"
                        | "PREPARE"
                )
            {
                return Err(OrmError::Migration(
                    "transaction control is not allowed in migration SQL".into(),
                ));
            }
            first = false;
            continue;
        }
        first = false;
        i += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_transaction_escape_without_rejecting_function_bodies() {
        for sql in [
            "COMMIT",
            "SELECT 1; /* nested /* comment */ */ ROLLBACK",
            "-- comment\nSTART TRANSACTION",
            "END",
            "SELECT '\\'; COMMIT; SELECT 'abc'; SELECT 1/0",
        ] {
            assert!(reject_transaction_control(sql).is_err());
        }
        for sql in [
            "DO $$ BEGIN RAISE NOTICE 'COMMIT'; END $$",
            "SELECT '; COMMIT'",
            "CREATE FUNCTION f() RETURNS void AS $body$ BEGIN RETURN; END $body$ LANGUAGE plpgsql",
        ] {
            reject_transaction_control(sql).unwrap();
        }
    }
}
