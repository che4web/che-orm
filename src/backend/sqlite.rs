use crate::{CompiledQuery, DatabaseValue, ExecuteResult, MigrationGraph, OrmError, Row};
use deadpool_sqlite::{Config, Object, Runtime};
use std::sync::Arc;
use tokio::sync::Mutex;

pub(crate) type Pool = deadpool_sqlite::Pool;
pub(crate) const MAX_PARAMETERS: usize = 900;

pub(crate) async fn connect(url: String, size: usize) -> Result<Pool, OrmError> {
    if size == 0 {
        return Err(OrmError::Configuration("pool size must be positive".into()));
    }
    let path = url.strip_prefix("sqlite://").unwrap_or(&url);
    if path == ":memory:" && size != 1 {
        return Err(OrmError::Configuration(
            "an in-memory SQLite database requires pool size 1".into(),
        ));
    }
    let mut config = Config::new(path);
    config.pool = Some(deadpool_sqlite::PoolConfig::new(size));
    let pool = config
        .create_pool(Runtime::Tokio1)
        .map_err(|error| OrmError::Configuration(error.to_string()))?;
    Session::acquire(&pool).await?;
    Ok(pool)
}

#[derive(Clone)]
pub(crate) struct Session(Arc<Mutex<Object>>);

impl Session {
    pub(crate) async fn acquire(pool: &Pool) -> Result<Self, OrmError> {
        let object = tokio::time::timeout(std::time::Duration::from_secs(30), pool.get())
            .await
            .map_err(|_| OrmError::Pool("connection acquisition timed out".into()))?
            .map_err(|error| OrmError::Pool(error.to_string()))?;
        let session = Self(Arc::new(Mutex::new(object)));
        session
            .interact(|connection| {
                if !connection.is_autocommit() {
                    connection.execute_batch("ROLLBACK")?;
                }
                connection.execute_batch("PRAGMA foreign_keys = ON")?;
                Ok(())
            })
            .await?;
        Ok(session)
    }

    pub(crate) async fn interact<T, F>(&self, action: F) -> Result<T, OrmError>
    where
        T: Send + 'static,
        F: FnOnce(&mut rusqlite::Connection) -> Result<T, OrmError> + Send + 'static,
    {
        self.0
            .lock()
            .await
            .interact(action)
            .await
            .map_err(|error| OrmError::Interaction(error.to_string()))?
    }

    pub(crate) async fn batch(&self, sql: &str) -> Result<(), OrmError> {
        let sql = sql.to_owned();
        self.interact(move |connection| {
            connection.execute_batch(&sql)?;
            Ok(())
        })
        .await
    }
    pub(crate) async fn commit(&self) -> Result<(), OrmError> {
        self.batch("COMMIT").await
    }

    pub(crate) async fn execute(&self, query: CompiledQuery) -> Result<ExecuteResult, OrmError> {
        self.interact(move |connection| {
            let params = params(&query)?;
            let rows_affected =
                connection.execute(&query.sql, rusqlite::params_from_iter(params))?;
            Ok(ExecuteResult {
                rows_affected,
                last_insert_rowid: if query.sql.starts_with("INSERT") {
                    Some(connection.last_insert_rowid())
                } else {
                    None
                },
            })
        })
        .await
    }

    pub(crate) async fn query(&self, query: CompiledQuery) -> Result<Vec<Row>, OrmError> {
        self.interact(move |connection| {
            let params = params(&query)?;
            let mut statement = connection.prepare(&query.sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(params), Row::from_sqlite)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
        .await
    }

    pub(crate) async fn migrate(
        &self,
        graph: MigrationGraph,
        target: Option<crate::MigrationId>,
    ) -> Result<usize, OrmError> {
        self.interact(move |connection| {
            crate::migration::sqlite_executor::apply_operation_migrations_to_on_connection(
                connection, graph, target,
            )
            .map_err(|error| OrmError::Migration(error.to_string()))
        })
        .await
    }

    pub(crate) async fn history(
        &self,
    ) -> Result<std::collections::HashMap<(String, String), String>, OrmError> {
        self.interact(|connection| {
            crate::migration::sqlite_executor::applied_operation_migrations(connection)
                .map_err(|error| OrmError::Migration(error.to_string()))
        })
        .await
    }
}

fn params(query: &CompiledQuery) -> Result<Vec<rusqlite::types::Value>, OrmError> {
    use rusqlite::types::Value;
    query
        .params
        .iter()
        .map(|value| {
            Ok(match value {
                DatabaseValue::Integer(value) => Value::Integer(*value),
                DatabaseValue::Real(value) => Value::Real(*value),
                DatabaseValue::Text(value) => Value::Text(value.clone()),
                DatabaseValue::Binary(value) => Value::Blob(value.clone()),
                DatabaseValue::Boolean(value) => Value::Integer(i64::from(*value)),
                DatabaseValue::Uuid(value) => Value::Blob(value.as_bytes().to_vec()),
                DatabaseValue::DateTime(value) => Value::Text(
                    value
                        .to_offset(time::UtcOffset::UTC)
                        .format(&time::format_description::well_known::Rfc3339)
                        .map_err(|error| OrmError::Interaction(error.to_string()))?,
                ),
                DatabaseValue::Null => Value::Null,
            })
        })
        .collect()
}
