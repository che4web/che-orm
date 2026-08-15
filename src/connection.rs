use deadpool_sqlite::{Config, Pool, Runtime};
use rusqlite::types::Value;
use time::format_description::well_known::Rfc3339;

use crate::{
    CompiledQuery, DatabaseValue, Model, ModelField, QueryAst, QueryBuildError, QueryValue,
    SqlCompiler, SqliteDialect,
};

#[derive(Debug, thiserror::Error)]
/// Errors returned by the SQLite pool and ORM runtime.
pub enum OrmError {
    #[error("database pool error: {0}")]
    Pool(#[from] deadpool_sqlite::PoolError),
    #[error("database interaction error: {0}")]
    Interaction(String),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("query build error: {0:?}")]
    QueryBuild(QueryBuildError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Result metadata returned by an executed mutation or DDL statement.
pub struct ExecuteResult {
    pub rows_affected: usize,
    pub last_insert_rowid: Option<i64>,
}

#[derive(Clone)]
/// Async SQLite database backed by a `deadpool-sqlite` connection pool.
pub struct Database {
    pool: Pool,
}

impl Database {
    /// Returns the underlying SQLite pool for advanced driver-specific use.
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Opens a SQLite database with a pool size of four connections.
    pub fn connect(path: impl Into<String>) -> Result<Self, OrmError> {
        Self::connect_with_pool_size(path, 4)
    }

    /// Opens the database configured by [`crate::settings::database_path`].
    pub fn connect_configured() -> Result<Self, OrmError> {
        Self::connect(crate::settings::database_path())
    }

    /// Opens a SQLite database with an explicit maximum pool size.
    pub fn connect_with_pool_size(
        path: impl Into<String>,
        pool_size: usize,
    ) -> Result<Self, OrmError> {
        let config = Config::new(path.into());
        let mut config = config;
        config.pool = Some(deadpool_sqlite::PoolConfig::new(pool_size));
        let pool = config
            .create_pool(Runtime::Tokio1)
            .map_err(|error| OrmError::Interaction(error.to_string()))?;
        Ok(Self { pool })
    }

    /// Opens an in-memory SQLite database with a single connection.
    pub fn connect_in_memory() -> Result<Self, OrmError> {
        Self::connect_with_pool_size(":memory:", 1)
    }

    /// Executes a compiled AST query asynchronously.
    pub async fn execute(&self, ast: QueryAst) -> Result<ExecuteResult, OrmError> {
        ast.validate().map_err(OrmError::QueryBuild)?;
        let compiled = SqlCompiler::<SqliteDialect>::compile(&ast);
        self.execute_compiled(compiled).await
    }

    /// Creates a model table and its indexes.
    pub async fn create_table<M: Model>(&self) -> Result<ExecuteResult, OrmError> {
        let schema = M::schema();
        let compiled = SqlCompiler::<SqliteDialect>::compile_schema(&schema);
        let pool = self.pool.clone();
        pool.get()
            .await?
            .interact(move |connection| -> rusqlite::Result<ExecuteResult> {
                configure_connection(connection)?;
                connection.execute_batch(&compiled.table)?;
                for index in compiled.indexes {
                    connection.execute_batch(&index)?;
                }
                Ok(ExecuteResult {
                    rows_affected: 0,
                    last_insert_rowid: None,
                })
            })
            .await
            .map_err(|error| OrmError::Interaction(error.to_string()))?
            .map_err(OrmError::from)
    }

    /// Inserts a model. Primary key and managed timestamp fields are omitted.
    pub async fn insert<M: Model>(&self, model: &M) -> Result<ExecuteResult, OrmError> {
        let ast = QueryAst::Insert(crate::InsertAst {
            table: crate::TableRef::new(M::table_name()),
            values: model.insert_values(),
            returning: Vec::new(),
        });
        self.execute(ast).await
    }

    /// Fetches all rows matching a typed select query.
    pub async fn fetch_all<M: Model + Send + 'static>(
        &self,
        query: crate::SelectQuery<M>,
    ) -> Result<Vec<M>, OrmError> {
        let ast = query.into_ast().map_err(OrmError::QueryBuild)?;
        let compiled = SqlCompiler::<SqliteDialect>::compile(&ast);
        let pool = self.pool.clone();
        pool.get()
            .await?
            .interact(move |connection| {
                configure_connection(connection)?;
                let mut statement = connection.prepare(&compiled.sql)?;
                let params = sqlite_params(&compiled)?;
                let rows = statement.query_map(rusqlite::params_from_iter(params), M::from_row)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .await
            .map_err(|error| OrmError::Interaction(error.to_string()))?
            .map_err(OrmError::from)
    }

    /// Fetches at most one row matching a typed select query.
    pub async fn fetch_one<M: Model + Send + 'static>(
        &self,
        query: crate::SelectQuery<M>,
    ) -> Result<Option<M>, OrmError> {
        let mut rows = self.fetch_all(query.limit(1)).await?;
        Ok(rows.pop())
    }

    /// Fetches related rows by a typed model field.
    ///
    /// This is the low-level building block for `has_many` and `belongs_to`:
    /// pass the foreign-key field on the target model and the owner key.
    pub async fn fetch_by<M, T, V>(
        &self,
        field: ModelField<M, T>,
        value: V,
    ) -> Result<Vec<M>, OrmError>
    where
        M: Model + Send + 'static,
        V: QueryValue<T>,
    {
        self.fetch_all(M::query().filter(field.eq(value))).await
    }

    /// Runs a blocking SQLite closure in a pool worker transaction.
    pub async fn transaction<T, F>(&self, action: F) -> Result<T, OrmError>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let pool = self.pool.clone();
        pool.get()
            .await?
            .interact(move |connection| -> rusqlite::Result<T> {
                configure_connection(connection)?;
                connection.execute_batch("BEGIN")?;
                match action(connection) {
                    Ok(value) => {
                        connection.execute_batch("COMMIT")?;
                        Ok(value)
                    }
                    Err(error) => {
                        let _ = connection.execute_batch("ROLLBACK");
                        Err(error)
                    }
                }
            })
            .await
            .map_err(|error| OrmError::Interaction(error.to_string()))?
            .map_err(OrmError::from)
    }

    async fn execute_compiled(&self, compiled: CompiledQuery) -> Result<ExecuteResult, OrmError> {
        let pool = self.pool.clone();
        pool.get()
            .await?
            .interact(move |connection| -> rusqlite::Result<ExecuteResult> {
                configure_connection(connection)?;
                let params = sqlite_params(&compiled)?;
                let rows_affected =
                    connection.execute(&compiled.sql, rusqlite::params_from_iter(params))?;
                Ok(ExecuteResult {
                    rows_affected,
                    last_insert_rowid: Some(connection.last_insert_rowid()),
                })
            })
            .await
            .map_err(|error| OrmError::Interaction(error.to_string()))?
            .map_err(OrmError::from)
    }
}

fn configure_connection(connection: &rusqlite::Connection) -> rusqlite::Result<()> {
    connection.execute_batch("PRAGMA foreign_keys = ON")
}

fn sqlite_params(compiled: &CompiledQuery) -> rusqlite::Result<Vec<Value>> {
    compiled.params.iter().map(database_value).collect()
}

fn database_value(value: &DatabaseValue) -> rusqlite::Result<Value> {
    match value {
        DatabaseValue::Integer(value) => Ok(Value::Integer(*value)),
        DatabaseValue::Text(value) => Ok(Value::Text(value.clone())),
        DatabaseValue::Boolean(value) => Ok(Value::Integer(i64::from(*value))),
        DatabaseValue::DateTime(value) => value
            .format(&Rfc3339)
            .map(Value::Text)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into())),
        DatabaseValue::Null => Ok(Value::Null),
    }
}
