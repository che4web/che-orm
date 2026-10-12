use crate::{
    BelongsTo, CompiledQuery, CurrentDialect, DatabaseValue, Expr, HasMany, MigrationGraph, Model,
    ModelField, QueryAst, QueryBuildError, QueryValue, Row, SelectQuery, SqlCompiler,
    ValidatedWrite, backend,
};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    marker::PhantomData,
    pin::Pin,
};

#[derive(Debug, thiserror::Error)]
pub enum OrmError {
    #[error("database configuration error: {0}")]
    Configuration(String),
    #[error("database pool error: {0}")]
    Pool(String),
    #[error("database interaction error: {0}")]
    Interaction(String),
    #[cfg(feature = "sqlite")]
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[cfg(feature = "postgres")]
    #[error("postgres error: {0}")]
    Postgres(#[from] tokio_postgres::Error),
    #[error("row decoding error: {0}")]
    Decode(#[from] crate::DecodeError),
    #[error("query build error: {0:?}")]
    QueryBuild(QueryBuildError),
    #[error("migration error: {0}")]
    Migration(String),
}

impl From<QueryBuildError> for OrmError {
    fn from(value: QueryBuildError) -> Self {
        Self::QueryBuild(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintKind {
    Unique,
    ForeignKey,
    NotNull,
    Check,
}

impl OrmError {
    pub fn constraint_kind(&self) -> Option<ConstraintKind> {
        #[cfg(feature = "postgres")]
        if let Some(code) = self.sql_state() {
            return match code {
                "23505" => Some(ConstraintKind::Unique),
                "23503" => Some(ConstraintKind::ForeignKey),
                "23502" => Some(ConstraintKind::NotNull),
                "23514" => Some(ConstraintKind::Check),
                _ => None,
            };
        }
        #[cfg(feature = "sqlite")]
        if let Self::Sqlite(rusqlite::Error::SqliteFailure(error, _)) = self {
            return match error.extended_code {
                rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                | rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY => Some(ConstraintKind::Unique),
                rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY => Some(ConstraintKind::ForeignKey),
                rusqlite::ffi::SQLITE_CONSTRAINT_NOTNULL => Some(ConstraintKind::NotNull),
                rusqlite::ffi::SQLITE_CONSTRAINT_CHECK => Some(ConstraintKind::Check),
                _ => None,
            };
        }
        None
    }
    /// PostgreSQL SQLSTATE, when the error originated from the server.
    pub fn sql_state(&self) -> Option<&str> {
        #[cfg(feature = "postgres")]
        if let Self::Postgres(error) = self {
            return error.code().map(|code| code.code());
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecuteResult {
    pub rows_affected: usize,
    /// SQLite metadata only. Use CRUD `RETURNING` models for portable keys.
    pub last_insert_rowid: Option<i64>,
}

/// Async database using the backend selected by the crate feature.
#[derive(Clone)]
pub struct Database {
    pool: backend::Pool,
    runtime: Option<std::sync::Arc<dyn ModelRuntime>>,
}

pub type TransactionFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, OrmError>> + Send + 'a>>;

/// Transactional extension point for registered model writes. Implementations
/// use raw transaction methods internally to avoid recursively capturing writes.
pub trait ModelRuntime: Send + Sync {
    fn role(&self, table: &str) -> Option<crate::ModelRole>;
    fn transaction_lock(&self) -> Option<std::sync::Arc<tokio::sync::Mutex<()>>> {
        None
    }
    fn prepare_read(&self, ast: QueryAst) -> QueryAst {
        ast
    }
    /// Recheck access for every query, including counts and relation loading.
    fn authorize_read(&self, _ast: &QueryAst) -> Result<(), OrmError> {
        Ok(())
    }
    fn write<'a>(
        &'a self,
        tx: &'a Transaction,
        ast: QueryAst,
    ) -> TransactionFuture<'a, ModelWriteResult>;
}

pub struct ModelWriteResult {
    pub result: ExecuteResult,
    pub rows: Vec<Row>,
}

fn write_table(ast: &QueryAst) -> Option<&str> {
    match ast {
        QueryAst::Insert(ast) => Some(ast.table.name),
        QueryAst::Update(ast) => Some(ast.table.name),
        QueryAst::Delete(ast) => Some(ast.table.name),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationLevel {
    ReadCommitted,
    RepeatableRead,
    Serializable,
}

impl Database {
    pub async fn connect(url: impl Into<String>) -> Result<Self, OrmError> {
        let url = url.into();
        let size = if url == ":memory:" || url == "sqlite://:memory:" {
            1
        } else {
            4
        };
        Self::connect_with_pool_size(url, size).await
    }
    pub async fn connect_with_pool_size(
        url: impl Into<String>,
        size: usize,
    ) -> Result<Self, OrmError> {
        Ok(Self {
            pool: backend::connect(url.into(), size).await?,
            runtime: None,
        })
    }
    #[cfg(feature = "sqlite")]
    pub async fn connect_in_memory() -> Result<Self, OrmError> {
        Self::connect(":memory:").await
    }

    pub async fn execute(&self, ast: QueryAst) -> Result<ExecuteResult, OrmError> {
        ast.validate()?;
        let ast = match (&self.runtime, ast) {
            (Some(runtime), ast @ QueryAst::Select(_)) => {
                runtime.authorize_read(&ast)?;
                runtime.prepare_read(ast)
            }
            (_, ast) => ast,
        };
        if self.captures(&ast) {
            return self
                .transaction(move |tx| Box::pin(async move { tx.execute(ast).await }))
                .await;
        }
        backend::Session::acquire(&self.pool)
            .await?
            .execute(SqlCompiler::<CurrentDialect>::compile(&ast))
            .await
    }

    /// Bind model-aware behavior to this database handle and its transactions.
    pub fn with_model_runtime(mut self, runtime: std::sync::Arc<dyn ModelRuntime>) -> Self {
        self.runtime = Some(runtime);
        self
    }

    pub fn model_role<M: Model>(&self) -> Option<crate::ModelRole> {
        self.runtime
            .as_ref()
            .and_then(|runtime| runtime.role(M::table_name()))
    }

    fn captures(&self, ast: &QueryAst) -> bool {
        write_table(ast).is_some_and(|table| {
            self.runtime
                .as_ref()
                .is_some_and(|runtime| runtime.role(table).is_some())
        })
    }
    /// Executes explicitly supplied SQL with bound values. SQL syntax is backend-specific.
    pub async fn execute_sql(
        &self,
        sql: impl Into<String>,
        params: Vec<DatabaseValue>,
    ) -> Result<ExecuteResult, OrmError> {
        backend::Session::acquire(&self.pool)
            .await?
            .execute(CompiledQuery {
                sql: sql.into(),
                params,
            })
            .await
    }
    pub async fn query_sql(
        &self,
        sql: impl Into<String>,
        params: Vec<DatabaseValue>,
    ) -> Result<Vec<Row>, OrmError> {
        backend::Session::acquire(&self.pool)
            .await?
            .query(CompiledQuery {
                sql: sql.into(),
                params,
            })
            .await
    }
    pub async fn create_table<M: Model>(&self) -> Result<ExecuteResult, OrmError> {
        M::schema().validate().map_err(|error| {
            OrmError::QueryBuild(QueryBuildError::InvalidSchema(format!("{error:?}")))
        })?;
        let schema = SqlCompiler::<CurrentDialect>::compile_schema(&M::schema());
        self.transaction(move |tx| {
            Box::pin(async move {
                tx.session.batch(&schema.table).await?;
                for sql in schema.indexes {
                    tx.session.batch(&sql).await?;
                }
                Ok(ExecuteResult {
                    rows_affected: 0,
                    last_insert_rowid: None,
                })
            })
        })
        .await
    }
    /// Inserts supplied UUID keys, omitting generated integer keys and managed timestamps.
    pub async fn insert<M: Model>(&self, model: &M) -> Result<ExecuteResult, OrmError> {
        self.execute(insert_ast(model)).await
    }
    pub async fn fetch_all<M: Model + Send + 'static>(
        &self,
        query: SelectQuery<M>,
    ) -> Result<Vec<M>, OrmError> {
        decode_models(self.rows(query.into_ast()?).await?)
    }
    pub async fn fetch_one<M: Model + Send + 'static>(
        &self,
        query: SelectQuery<M>,
    ) -> Result<Option<M>, OrmError> {
        Ok(self.fetch_all(query.limit(1)).await?.pop())
    }
    pub async fn get<M: Model + Send + 'static>(
        &self,
        id: M::PrimaryKey,
    ) -> Result<Option<M>, OrmError> {
        self.fetch_one(M::query().filter(M::primary_key().eq(id)))
            .await
    }
    pub async fn all<M: Model + Send + 'static>(&self) -> Result<Vec<M>, OrmError> {
        self.fetch_all(M::query()).await
    }
    pub async fn count<M: Model + Send + 'static>(&self) -> Result<usize, OrmError> {
        self.count_query(M::query()).await
    }
    pub async fn count_query<M: Model + Send + 'static>(
        &self,
        query: SelectQuery<M>,
    ) -> Result<usize, OrmError> {
        let query = count_query(query, self.runtime.as_deref())?;
        count_rows(
            backend::Session::acquire(&self.pool)
                .await?
                .query(query)
                .await?,
        )
    }
    pub fn create<M: Model>(&self) -> CreateBuilder<'_, M> {
        CreateBuilder {
            database: self,
            query: M::insert(),
        }
    }
    pub fn update<M: Model>(&self, id: M::PrimaryKey) -> UpdateBuilder<'_, M> {
        UpdateBuilder {
            database: self,
            query: M::update().filter(M::primary_key().eq(id)),
        }
    }
    pub async fn delete<M: Model + Send + 'static>(
        &self,
        id: M::PrimaryKey,
    ) -> Result<bool, OrmError> {
        Ok(self
            .execute(M::delete().filter(M::primary_key().eq(id)).into_ast()?)
            .await?
            .rows_affected
            == 1)
    }
    pub fn query<M: Model>(&self) -> DatabaseQuery<M> {
        DatabaseQuery::new(M::query())
    }
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
    pub async fn fetch_by_many<M, T, I, V>(
        &self,
        field: ModelField<M, T>,
        values: I,
    ) -> Result<Vec<M>, OrmError>
    where
        M: Model + Send + 'static,
        I: IntoIterator<Item = V>,
        V: QueryValue<T>,
    {
        let values = values
            .into_iter()
            .map(QueryValue::<T>::into_query_value)
            .collect::<Vec<_>>();
        let mut result = Vec::new();
        for chunk in values.chunks(backend::MAX_PARAMETERS) {
            result.extend(
                self.fetch_all(M::query().filter(Expr::In {
                    left: Box::new(Expr::Column(field.column())),
                    values: chunk.to_vec(),
                }))
                .await?,
            );
        }
        let mut seen = HashSet::new();
        result.retain(|model| seen.insert(model.primary_key_value()));
        Ok(result)
    }

    /// Runs one serializable async ORM transaction. Unfinished transactions are
    /// rolled back before a cancelled transaction's connection is recycled.
    /// Use `|tx| Box::pin(async move { ... })` and await transaction methods.
    pub async fn transaction<T, F>(&self, action: F) -> Result<T, OrmError>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a Transaction) -> TransactionFuture<'a, T> + Send,
    {
        self.transaction_with_isolation(IsolationLevel::Serializable, action)
            .await
    }

    /// SQLite uses its serializable snapshot for every requested isolation level.
    pub async fn transaction_with_isolation<T, F>(
        &self,
        isolation: IsolationLevel,
        action: F,
    ) -> Result<T, OrmError>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a Transaction) -> TransactionFuture<'a, T> + Send,
    {
        let _runtime_guard = match self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.transaction_lock())
        {
            Some(lock) => Some(lock.lock_owned().await),
            None => None,
        };
        let session = backend::Session::acquire(&self.pool).await?;
        let mut guard = TransactionGuard {
            session: session.clone(),
            active: true,
        };
        #[cfg(feature = "sqlite")]
        let begin = {
            let _ = isolation;
            "BEGIN"
        };
        #[cfg(all(feature = "postgres", not(feature = "sqlite")))]
        let begin = match isolation {
            IsolationLevel::ReadCommitted => "BEGIN ISOLATION LEVEL READ COMMITTED",
            IsolationLevel::RepeatableRead => "BEGIN ISOLATION LEVEL REPEATABLE READ",
            IsolationLevel::Serializable => "BEGIN ISOLATION LEVEL SERIALIZABLE",
        };
        session.batch(begin).await?;
        let transaction = Transaction {
            session,
            runtime: self.runtime.clone(),
        };
        match action(&transaction).await {
            Ok(value) => {
                transaction.session.commit().await?;
                guard.active = false;
                Ok(value)
            }
            Err(error) => {
                transaction.session.batch("ROLLBACK").await?;
                guard.active = false;
                Err(error)
            }
        }
    }
    pub async fn typed_transaction<T, F>(&self, action: F) -> Result<T, OrmError>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a Transaction) -> TransactionFuture<'a, T> + Send,
    {
        self.transaction(action).await
    }

    /// SQLite-only escape hatch for direct driver operations.
    #[cfg(feature = "sqlite")]
    pub async fn raw_transaction<T, F>(&self, action: F) -> Result<T, OrmError>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        self.transaction(move |tx| {
            Box::pin(async move {
                tx.session
                    .interact(move |connection| action(connection).map_err(Into::into))
                    .await
            })
        })
        .await
    }
    pub async fn migrate(&self, graph: MigrationGraph) -> Result<usize, OrmError> {
        self.migrate_to(graph, None).await
    }
    pub async fn migrate_to(
        &self,
        graph: MigrationGraph,
        target: Option<crate::MigrationId>,
    ) -> Result<usize, OrmError> {
        backend::Session::acquire(&self.pool)
            .await?
            .migrate(graph, target)
            .await
    }
    pub async fn migration_history(&self) -> Result<HashMap<(String, String), String>, OrmError> {
        backend::Session::acquire(&self.pool).await?.history().await
    }
    async fn rows(&self, ast: QueryAst) -> Result<Vec<Row>, OrmError> {
        ast.validate()?;
        if self.captures(&ast) {
            return self
                .transaction(move |tx| Box::pin(async move { tx.rows(ast).await }))
                .await;
        }
        let ast = match &self.runtime {
            Some(runtime) => {
                runtime.authorize_read(&ast)?;
                runtime.prepare_read(ast)
            }
            None => ast,
        };
        backend::Session::acquire(&self.pool)
            .await?
            .query(SqlCompiler::<CurrentDialect>::compile(&ast))
            .await
    }
    async fn execute_returning<M: Model + Send + 'static>(
        &self,
        ast: QueryAst,
    ) -> Result<Option<M>, OrmError> {
        Ok(decode_models(self.rows(ast).await?)?.pop())
    }
}

pub(crate) struct TransactionGuard {
    pub(crate) session: backend::Session,
    pub(crate) active: bool,
}
impl Drop for TransactionGuard {
    fn drop(&mut self) {
        if self.active {
            let session = self.session.clone();
            tokio::spawn(async move {
                let _ = session.batch("ROLLBACK").await;
            });
        }
    }
}

/// ORM operations pinned to one transaction connection on either backend.
pub struct Transaction {
    session: backend::Session,
    runtime: Option<std::sync::Arc<dyn ModelRuntime>>,
}
impl Transaction {
    /// Executes backend-specific SQL on this transaction with bound values.
    pub async fn execute_sql(
        &self,
        sql: impl Into<String>,
        params: Vec<DatabaseValue>,
    ) -> Result<ExecuteResult, OrmError> {
        self.session
            .execute(CompiledQuery {
                sql: sql.into(),
                params,
            })
            .await
    }

    /// Queries backend-specific SQL on this transaction with bound values.
    pub async fn query_sql(
        &self,
        sql: impl Into<String>,
        params: Vec<DatabaseValue>,
    ) -> Result<Vec<Row>, OrmError> {
        self.session
            .query(CompiledQuery {
                sql: sql.into(),
                params,
            })
            .await
    }

    pub async fn execute(&self, ast: QueryAst) -> Result<ExecuteResult, OrmError> {
        ast.validate()?;
        let ast = match (&self.runtime, ast) {
            (Some(runtime), ast @ QueryAst::Select(_)) => {
                runtime.authorize_read(&ast)?;
                runtime.prepare_read(ast)
            }
            (_, ast) => ast,
        };
        if let Some(runtime) = &self.runtime {
            if write_table(&ast).is_some_and(|table| runtime.role(table).is_some()) {
                return Ok(runtime.write(self, ast).await?.result);
            }
        }
        self.session
            .execute(SqlCompiler::<CurrentDialect>::compile(&ast))
            .await
    }
    pub async fn insert<M: Model>(&self, model: &M) -> Result<ExecuteResult, OrmError> {
        self.execute(insert_ast(model)).await
    }
    pub async fn fetch_all<M: Model + Send + 'static>(
        &self,
        query: SelectQuery<M>,
    ) -> Result<Vec<M>, OrmError> {
        decode_models(self.rows(query.into_ast()?).await?)
    }
    pub async fn fetch_one<M: Model + Send + 'static>(
        &self,
        query: SelectQuery<M>,
    ) -> Result<Option<M>, OrmError> {
        Ok(self.fetch_all(query.limit(1)).await?.pop())
    }
    pub async fn get<M: Model + Send + 'static>(
        &self,
        id: M::PrimaryKey,
    ) -> Result<Option<M>, OrmError> {
        self.fetch_one(M::query().filter(M::primary_key().eq(id)))
            .await
    }
    pub async fn all<M: Model + Send + 'static>(&self) -> Result<Vec<M>, OrmError> {
        self.fetch_all(M::query()).await
    }
    pub async fn count<M: Model + Send + 'static>(&self) -> Result<usize, OrmError> {
        count_rows(
            self.session
                .query(count_query(M::query(), self.runtime.as_deref())?)
                .await?,
        )
    }
    pub async fn delete<M: Model + Send + 'static>(
        &self,
        id: M::PrimaryKey,
    ) -> Result<bool, OrmError> {
        Ok(self
            .execute(M::delete().filter(M::primary_key().eq(id)).into_ast()?)
            .await?
            .rows_affected
            == 1)
    }
    pub fn create<M: Model>(&self) -> TransactionCreateBuilder<'_, M> {
        TransactionCreateBuilder {
            transaction: self,
            query: M::insert(),
        }
    }
    pub fn update<M: Model>(&self, id: M::PrimaryKey) -> TransactionUpdateBuilder<'_, M> {
        TransactionUpdateBuilder {
            transaction: self,
            query: M::update().filter(M::primary_key().eq(id)),
        }
    }
    pub fn query<M: Model>(&self) -> TransactionQuery<'_, M> {
        TransactionQuery {
            transaction: self,
            query: M::query(),
        }
    }
    pub async fn fetch_by_many<M, T, I, V>(
        &self,
        field: ModelField<M, T>,
        values: I,
    ) -> Result<Vec<M>, OrmError>
    where
        M: Model + Send + 'static,
        I: IntoIterator<Item = V>,
        V: QueryValue<T>,
    {
        let values = values
            .into_iter()
            .map(QueryValue::<T>::into_query_value)
            .collect::<Vec<_>>();
        let mut result = Vec::new();
        for chunk in values.chunks(backend::MAX_PARAMETERS) {
            result.extend(
                self.fetch_all(M::query().filter(Expr::In {
                    left: Box::new(Expr::Column(field.column())),
                    values: chunk.to_vec(),
                }))
                .await?,
            );
        }
        let mut seen = HashSet::new();
        result.retain(|model| seen.insert(model.primary_key_value()));
        Ok(result)
    }
    async fn rows(&self, ast: QueryAst) -> Result<Vec<Row>, OrmError> {
        ast.validate()?;
        if let Some(runtime) = &self.runtime {
            if write_table(&ast).is_some_and(|table| runtime.role(table).is_some()) {
                return Ok(runtime.write(self, ast).await?.rows);
            }
        }
        let ast = match &self.runtime {
            Some(runtime) => {
                runtime.authorize_read(&ast)?;
                runtime.prepare_read(ast)
            }
            None => ast,
        };
        self.session
            .query(SqlCompiler::<CurrentDialect>::compile(&ast))
            .await
    }
    async fn execute_returning<M: Model + Send + 'static>(
        &self,
        ast: QueryAst,
    ) -> Result<Option<M>, OrmError> {
        Ok(decode_models(self.rows(ast).await?)?.pop())
    }
}

fn insert_ast<M: Model>(model: &M) -> QueryAst {
    QueryAst::Insert(crate::InsertAst {
        table: crate::TableRef::new(M::table_name()),
        values: model.insert_values(),
        returning: Vec::new(),
    })
}
fn count_query<M: Model>(
    query: SelectQuery<M>,
    runtime: Option<&dyn ModelRuntime>,
) -> Result<CompiledQuery, OrmError> {
    let ast = QueryAst::Select(query.into_select_ast()?);
    let ast = match runtime {
        Some(runtime) => {
            runtime.authorize_read(&ast)?;
            runtime.prepare_read(ast)
        }
        None => ast,
    };
    let compiled = SqlCompiler::<CurrentDialect>::compile(&ast);
    Ok(CompiledQuery {
        sql: format!("SELECT COUNT(*) FROM ({}) AS che_count", compiled.sql),
        params: compiled.params,
    })
}
fn count_rows(rows: Vec<Row>) -> Result<usize, OrmError> {
    let count: i64 = rows
        .first()
        .ok_or_else(|| OrmError::Interaction("count returned no row".into()))?
        .get(0)?;
    usize::try_from(count).map_err(|error| OrmError::Interaction(error.to_string()))
}
fn decode_models<M: Model>(rows: Vec<Row>) -> Result<Vec<M>, OrmError> {
    rows.iter()
        .map(|row| M::from_row(row).map_err(Into::into))
        .collect()
}

pub struct TransactionCreateBuilder<'tx, M> {
    transaction: &'tx Transaction,
    query: crate::InsertQuery<M>,
}
impl<M: Model> TransactionCreateBuilder<'_, M> {
    pub fn set<T, V: QueryValue<T>>(self, field: ModelField<M, T>, value: V) -> Self {
        Self {
            query: self.query.set(field, value),
            ..self
        }
    }
    pub async fn execute(self) -> Result<M, OrmError>
    where
        M: Send + 'static,
    {
        self.transaction
            .execute_returning(self.query.returning_all().into_ast()?)
            .await?
            .ok_or_else(|| OrmError::Interaction("insert did not return a row".into()))
    }
}
pub struct TransactionUpdateBuilder<'tx, M> {
    transaction: &'tx Transaction,
    query: crate::UpdateQuery<M>,
}
impl<M: Model> TransactionUpdateBuilder<'_, M> {
    pub fn set<T, V: QueryValue<T>>(self, field: ModelField<M, T>, value: V) -> Self {
        Self {
            query: self.query.set(field, value),
            ..self
        }
    }
    pub async fn execute(self) -> Result<Option<M>, OrmError>
    where
        M: Send + 'static,
    {
        self.transaction
            .execute_returning(self.query.returning_all().into_ast()?)
            .await
    }
}
pub struct TransactionQuery<'tx, M> {
    transaction: &'tx Transaction,
    query: SelectQuery<M>,
}
impl<M: Model> TransactionQuery<'_, M> {
    pub fn filter(self, expr: Expr) -> Self {
        Self {
            query: self.query.filter(expr),
            ..self
        }
    }
    pub fn order_by(self, order: crate::OrderBy) -> Self {
        Self {
            query: self.query.order_by(order),
            ..self
        }
    }
    pub fn limit(self, limit: u64) -> Self {
        Self {
            query: self.query.limit(limit),
            ..self
        }
    }
    pub fn offset(self, offset: u64) -> Self {
        Self {
            query: self.query.offset(offset),
            ..self
        }
    }
    pub async fn all(self) -> Result<Vec<M>, OrmError>
    where
        M: Send + 'static,
    {
        self.transaction.fetch_all(self.query).await
    }
    pub async fn first(self) -> Result<Option<M>, OrmError>
    where
        M: Send + 'static,
    {
        self.transaction.fetch_one(self.query).await
    }
}
impl<M: Model + Send + 'static> ValidatedWrite<M> {
    pub async fn save(self, database: &Database) -> Result<Option<M>, OrmError> {
        let ast = match self {
            Self::Create(query) => query.returning_all().into_ast()?,
            Self::Update(query) => query.returning_all().into_ast()?,
        };
        database.execute_returning(ast).await
    }
}
pub struct CreateBuilder<'db, M> {
    database: &'db Database,
    query: crate::InsertQuery<M>,
}
impl<M: Model> CreateBuilder<'_, M> {
    pub fn set<T, V: QueryValue<T>>(self, field: ModelField<M, T>, value: V) -> Self {
        Self {
            query: self.query.set(field, value),
            ..self
        }
    }
    pub async fn execute(self) -> Result<M, OrmError>
    where
        M: Send + 'static,
    {
        self.database
            .execute_returning(self.query.returning_all().into_ast()?)
            .await?
            .ok_or_else(|| OrmError::Interaction("insert did not return a row".into()))
    }
}
pub struct UpdateBuilder<'db, M> {
    database: &'db Database,
    query: crate::UpdateQuery<M>,
}
impl<M: Model> UpdateBuilder<'_, M> {
    pub fn set<T, V: QueryValue<T>>(self, field: ModelField<M, T>, value: V) -> Self {
        Self {
            query: self.query.set(field, value),
            ..self
        }
    }
    pub async fn execute(self) -> Result<Option<M>, OrmError>
    where
        M: Send + 'static,
    {
        self.database
            .execute_returning(self.query.returning_all().into_ast()?)
            .await
    }
}
pub struct DatabaseQuery<M> {
    query: SelectQuery<M>,
}
impl<M: Model> DatabaseQuery<M> {
    pub fn new(query: SelectQuery<M>) -> Self {
        Self { query }
    }
    pub fn filter(self, expr: Expr) -> Self {
        Self {
            query: self.query.filter(expr),
        }
    }
    pub fn order_by(self, order: crate::OrderBy) -> Self {
        Self {
            query: self.query.order_by(order),
        }
    }
    pub fn limit(self, limit: u64) -> Self {
        Self {
            query: self.query.limit(limit),
        }
    }
    pub fn offset(self, offset: u64) -> Self {
        Self {
            query: self.query.offset(offset),
        }
    }
    pub fn select_related<R, Relation, Key>(
        self,
        relation: BelongsTo<M, R, Relation, Key>,
    ) -> SelectRelatedQuery<M, R, Relation, Key> {
        SelectRelatedQuery {
            query: self.query,
            relation,
        }
    }
    pub fn prefetch_related<P: PrefetchRelation<M>>(self, plan: P) -> P::Query {
        plan.into_query(self.query)
    }
    pub async fn all(self, db: &Database) -> Result<Vec<M>, OrmError>
    where
        M: Send + 'static,
    {
        db.fetch_all(self.query).await
    }
    pub async fn first(self, db: &Database) -> Result<Option<M>, OrmError>
    where
        M: Send + 'static,
    {
        db.fetch_one(self.query).await
    }
    pub async fn count(self, db: &Database) -> Result<usize, OrmError>
    where
        M: Send + 'static,
    {
        db.count_query(self.query).await
    }
    pub fn into_select_query(self) -> SelectQuery<M> {
        self.query
    }
}
pub trait PrefetchRelation<Owner: Model> {
    type Query;
    fn into_query(self, query: SelectQuery<Owner>) -> Self::Query;
}

#[derive(Debug)]
pub struct WithOne<M, R, Relation = (), Key = i64> {
    pub model: M,
    pub related: R,
    pub(crate) _relation: PhantomData<Relation>,
    pub(crate) _key: PhantomData<Key>,
}
#[derive(Debug)]
pub struct WithOptionalOne<M, R, Relation = ()> {
    pub model: M,
    pub related: Option<R>,
    pub(crate) _relation: PhantomData<Relation>,
}
#[derive(Debug)]
pub struct WithMany<M, R, Relation = (), Key = i64> {
    pub model: M,
    pub related: Vec<R>,
    pub(crate) _relation: PhantomData<Relation>,
    pub(crate) _key: PhantomData<Key>,
}
#[derive(Debug)]
pub struct Loaded<M, Relations> {
    pub model: M,
    pub relations: Relations,
}
#[derive(Debug)]
pub struct LoadedMany<R, Relation = ()> {
    pub related: Vec<R>,
    pub(crate) _relation: PhantomData<Relation>,
}
#[derive(Debug)]
pub struct LoadedOne<R, Relation = ()> {
    pub related: R,
    pub(crate) _relation: PhantomData<Relation>,
}

pub struct SelectRelatedQuery<M, R, Relation = (), Key = i64> {
    query: SelectQuery<M>,
    relation: BelongsTo<M, R, Relation, Key>,
}
impl<M: Model, R, Relation, Key> SelectRelatedQuery<M, R, Relation, Key> {
    pub fn filter(self, expr: Expr) -> Self {
        Self {
            query: self.query.filter(expr),
            ..self
        }
    }
    pub fn order_by(self, order: crate::OrderBy) -> Self {
        Self {
            query: self.query.order_by(order),
            ..self
        }
    }
    pub fn limit(self, limit: u64) -> Self {
        Self {
            query: self.query.limit(limit),
            ..self
        }
    }
    pub fn offset(self, offset: u64) -> Self {
        Self {
            query: self.query.offset(offset),
            ..self
        }
    }
    pub async fn count(self, db: &Database) -> Result<usize, OrmError>
    where
        M: Send + 'static,
        R: Model,
        Key: crate::ForeignKeyValue<R::PrimaryKey>,
    {
        let kind = if Key::OPTIONAL {
            crate::JoinType::Left
        } else {
            crate::JoinType::Inner
        };
        let ast = self.query.into_joined_ast_with_kind(self.relation, kind)?;
        let compiled = SqlCompiler::<CurrentDialect>::compile(&ast);
        count_rows(
            db.query_sql(
                format!("SELECT COUNT(*) FROM ({}) AS che_count", compiled.sql),
                compiled.params,
            )
            .await?,
        )
    }
}
impl<M, R, Relation, Key: crate::PrimaryKey> SelectRelatedQuery<M, R, Relation, Key>
where
    M: Model + Send + 'static,
    R: Model + Send + 'static,
{
    pub fn select_related<R2: Model, Relation2>(
        self,
        relation: BelongsTo<M, R2, Relation2, R2::PrimaryKey>,
    ) -> MultiSelectRelatedQuery<M, R, Relation, R2, Relation2, Key> {
        MultiSelectRelatedQuery {
            query: self.query,
            first: self.relation,
            second: relation,
        }
    }
    pub async fn all(self, db: &Database) -> Result<Vec<WithOne<M, R, Relation, Key>>, OrmError> {
        let offset = M::columns().len();
        db.rows(self.query.into_joined_ast(self.relation)?)
            .await?
            .iter()
            .map(|row| {
                Ok(WithOne {
                    model: M::from_row_at(row, 0)?,
                    related: R::from_row_at(row, offset)?,
                    _relation: PhantomData,
                    _key: PhantomData,
                })
            })
            .collect()
    }
}
impl<M, R, Relation, Key: crate::PrimaryKey> SelectRelatedQuery<M, R, Relation, Option<Key>>
where
    M: Model + Send + 'static,
    R: Model + Send + 'static,
{
    pub async fn all(
        self,
        db: &Database,
    ) -> Result<Vec<WithOptionalOne<M, R, Relation>>, OrmError> {
        let offset = M::columns().len();
        let pk_offset = R::columns()
            .iter()
            .position(|column| *column == R::primary_key().column().name)
            .ok_or_else(|| {
                OrmError::Interaction("primary key missing from model columns".into())
            })?;
        db.rows(self.query.into_optional_joined_ast(self.relation)?)
            .await?
            .iter()
            .map(|row| {
                let id: Option<R::PrimaryKey> = row.get(offset + pk_offset)?;
                Ok(WithOptionalOne {
                    model: M::from_row_at(row, 0)?,
                    related: id.map(|_| R::from_row_at(row, offset)).transpose()?,
                    _relation: PhantomData,
                })
            })
            .collect()
    }
}
pub struct MultiSelectRelatedQuery<M, R1, Relation1, R2: Model, Relation2, Key1 = i64> {
    query: SelectQuery<M>,
    first: BelongsTo<M, R1, Relation1, Key1>,
    second: BelongsTo<M, R2, Relation2, R2::PrimaryKey>,
}
impl<M, R1, Relation1, R2, Relation2, Key1>
    MultiSelectRelatedQuery<M, R1, Relation1, R2, Relation2, Key1>
where
    M: Model + Send + 'static,
    R1: Model + Send + 'static,
    R2: Model + Send + 'static,
{
    pub fn filter(self, expr: Expr) -> Self {
        Self {
            query: self.query.filter(expr),
            ..self
        }
    }
    pub fn order_by(self, order: crate::OrderBy) -> Self {
        Self {
            query: self.query.order_by(order),
            ..self
        }
    }
    pub async fn all(
        self,
        db: &Database,
    ) -> Result<Vec<Loaded<M, (LoadedOne<R1, Relation1>, LoadedOne<R2, Relation2>)>>, OrmError>
    {
        let second = self.second;
        let mut ast = self.query.into_joined_ast(self.first)?;
        if let QueryAst::Select(select) = &mut ast {
            select.columns.extend(
                R2::columns()
                    .iter()
                    .map(|column| crate::ColumnRef::new(second.alias(), column)),
            );
            select.joins.push(crate::JoinAst {
                table: crate::TableRef::new(R2::table_name()),
                alias: second.alias(),
                kind: crate::JoinType::Inner,
                on: crate::Expr::Compare {
                    left: Box::new(Expr::Column(second.field())),
                    op: crate::CompareOp::Eq,
                    right: Box::new(Expr::Column(crate::ColumnRef::new(
                        second.alias(),
                        R2::primary_key().column().name,
                    ))),
                },
            });
        }
        let offset = M::columns().len();
        db.rows(ast)
            .await?
            .iter()
            .map(|row| {
                Ok(Loaded {
                    model: M::from_row_at(row, 0)?,
                    relations: (
                        LoadedOne {
                            related: R1::from_row_at(row, offset)?,
                            _relation: PhantomData,
                        },
                        LoadedOne {
                            related: R2::from_row_at(row, offset + R1::columns().len())?,
                            _relation: PhantomData,
                        },
                    ),
                })
            })
            .collect()
    }
}

pub struct PrefetchRelatedQuery<M, R, Relation = (), Key = i64> {
    query: SelectQuery<M>,
    relation: HasMany<M, R, Relation, Key>,
}
impl<M: Model, R, Relation, Key: crate::ForeignKeyValue<M::PrimaryKey>> PrefetchRelation<M>
    for HasMany<M, R, Relation, Key>
{
    type Query = PrefetchRelatedQuery<M, R, Relation, Key>;
    fn into_query(self, query: SelectQuery<M>) -> Self::Query {
        PrefetchRelatedQuery {
            query,
            relation: self,
        }
    }
}
fn ids<M: Model>(models: &[M]) -> Vec<M::PrimaryKey> {
    models
        .iter()
        .map(Model::primary_key_value)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect()
}
fn group<M: Model, R, Relation, Key: crate::ForeignKeyValue<M::PrimaryKey>>(
    relation: &HasMany<M, R, Relation, Key>,
    models: Vec<R>,
) -> HashMap<M::PrimaryKey, Vec<R>> {
    let mut grouped = HashMap::<M::PrimaryKey, Vec<R>>::new();
    for model in models {
        if let Some(key) = crate::ForeignKeyValue::owner_key(relation.foreign_key(&model)) {
            grouped.entry(key).or_default().push(model);
        }
    }
    grouped
}
impl<M, R, Relation, Key> PrefetchRelatedQuery<M, R, Relation, Key>
where
    M: Model + Send + 'static,
    R: Model + Send + 'static,
    Key: crate::ForeignKeyValue<M::PrimaryKey>,
{
    pub fn filter(self, expr: Expr) -> Self {
        Self {
            query: self.query.filter(expr),
            ..self
        }
    }
    pub fn order_by(self, order: crate::OrderBy) -> Self {
        Self {
            query: self.query.order_by(order),
            ..self
        }
    }
    pub fn limit(self, limit: u64) -> Self {
        Self {
            query: self.query.limit(limit),
            ..self
        }
    }
    pub fn offset(self, offset: u64) -> Self {
        Self {
            query: self.query.offset(offset),
            ..self
        }
    }
    pub fn prefetch_related<R2, Relation2, Key2: crate::ForeignKeyValue<M::PrimaryKey>>(
        self,
        relation: HasMany<M, R2, Relation2, Key2>,
    ) -> MultiPrefetchRelatedQuery<M, R, Relation, R2, Relation2, Key, Key2> {
        MultiPrefetchRelatedQuery {
            query: self.query,
            first: self.relation,
            second: relation,
        }
    }
    pub async fn count(self, db: &Database) -> Result<usize, OrmError> {
        db.count_query(self.query).await
    }
    pub async fn all(
        self,
        db: &Database,
    ) -> Result<Vec<Loaded<M, (LoadedMany<R, Relation>,)>>, OrmError>
    where
        Relation: Send + 'static,
    {
        db.transaction(move |tx| {
            Box::pin(async move {
                let parents = tx.fetch_all(self.query).await?;
                let field = self.relation.field();
                let related = tx
                    .fetch_by_many(
                        ModelField::<R, M::PrimaryKey>::new(field.table, field.name),
                        ids(&parents),
                    )
                    .await?;
                let mut related = group(&self.relation, related);
                Ok(parents
                    .into_iter()
                    .map(|model| {
                        let key = model.primary_key_value();
                        Loaded {
                            model,
                            relations: (LoadedMany {
                                related: related.remove(&key).unwrap_or_default(),
                                _relation: PhantomData,
                            },),
                        }
                    })
                    .collect())
            })
        })
        .await
    }
}
pub struct MultiPrefetchRelatedQuery<
    M: Model,
    R1,
    Relation1,
    R2,
    Relation2,
    Key1 = <M as Model>::PrimaryKey,
    Key2 = <M as Model>::PrimaryKey,
> {
    query: SelectQuery<M>,
    first: HasMany<M, R1, Relation1, Key1>,
    second: HasMany<M, R2, Relation2, Key2>,
}
impl<M, R1, Relation1, R2, Relation2, Key1, Key2>
    MultiPrefetchRelatedQuery<M, R1, Relation1, R2, Relation2, Key1, Key2>
where
    M: Model + Send + 'static,
    R1: Model + Send + 'static,
    R2: Model + Send + 'static,
    Relation1: Send + 'static,
    Relation2: Send + 'static,
    Key1: crate::ForeignKeyValue<M::PrimaryKey>,
    Key2: crate::ForeignKeyValue<M::PrimaryKey>,
{
    pub async fn all(
        self,
        db: &Database,
    ) -> Result<Vec<Loaded<M, (LoadedMany<R1, Relation1>, LoadedMany<R2, Relation2>)>>, OrmError>
    {
        db.transaction(move |tx| {
            Box::pin(async move {
                let parents = tx.fetch_all(self.query).await?;
                let keys = ids(&parents);
                let field = self.first.field();
                let first = tx
                    .fetch_by_many(
                        ModelField::<R1, M::PrimaryKey>::new(field.table, field.name),
                        keys.clone(),
                    )
                    .await?;
                let field = self.second.field();
                let second = tx
                    .fetch_by_many(
                        ModelField::<R2, M::PrimaryKey>::new(field.table, field.name),
                        keys,
                    )
                    .await?;
                let mut first = group(&self.first, first);
                let mut second = group(&self.second, second);
                Ok(parents
                    .into_iter()
                    .map(|model| {
                        let key = model.primary_key_value();
                        Loaded {
                            model,
                            relations: (
                                LoadedMany {
                                    related: first.remove(&key).unwrap_or_default(),
                                    _relation: PhantomData,
                                },
                                LoadedMany {
                                    related: second.remove(&key).unwrap_or_default(),
                                    _relation: PhantomData,
                                },
                            ),
                        }
                    })
                    .collect())
            })
        })
        .await
    }
}
pub struct NestedPrefetchPlan<
    M: Model,
    R: Model,
    Relation,
    C,
    ChildRelation,
    OuterKey = <M as Model>::PrimaryKey,
    InnerKey = <R as Model>::PrimaryKey,
> {
    outer: HasMany<M, R, Relation, OuterKey>,
    inner: HasMany<R, C, ChildRelation, InnerKey>,
}
impl<M: Model, R: Model, Relation, Key: crate::ForeignKeyValue<M::PrimaryKey>>
    HasMany<M, R, Relation, Key>
{
    pub fn prefetch<C, ChildRelation, InnerKey: crate::ForeignKeyValue<R::PrimaryKey>>(
        self,
        inner: HasMany<R, C, ChildRelation, InnerKey>,
    ) -> NestedPrefetchPlan<M, R, Relation, C, ChildRelation, Key, InnerKey> {
        NestedPrefetchPlan { outer: self, inner }
    }
}
impl<
    M: Model,
    R: Model,
    Relation,
    C,
    ChildRelation,
    OuterKey: crate::ForeignKeyValue<M::PrimaryKey>,
    InnerKey: crate::ForeignKeyValue<R::PrimaryKey>,
> PrefetchRelation<M> for NestedPrefetchPlan<M, R, Relation, C, ChildRelation, OuterKey, InnerKey>
{
    type Query = NestedPrefetchQuery<M, R, Relation, C, ChildRelation, OuterKey, InnerKey>;
    fn into_query(self, query: SelectQuery<M>) -> Self::Query {
        NestedPrefetchQuery {
            query,
            outer: self.outer,
            inner: self.inner,
        }
    }
}
pub struct NestedPrefetchQuery<
    M: Model,
    R: Model,
    Relation,
    C,
    ChildRelation,
    OuterKey = <M as Model>::PrimaryKey,
    InnerKey = <R as Model>::PrimaryKey,
> {
    query: SelectQuery<M>,
    outer: HasMany<M, R, Relation, OuterKey>,
    inner: HasMany<R, C, ChildRelation, InnerKey>,
}
impl<M, R, Relation, C, ChildRelation, OuterKey, InnerKey>
    NestedPrefetchQuery<M, R, Relation, C, ChildRelation, OuterKey, InnerKey>
where
    M: Model + Send + 'static,
    R: Model + Send + 'static,
    C: Model + Send + 'static,
    Relation: Send + 'static,
    ChildRelation: Send + 'static,
    OuterKey: crate::ForeignKeyValue<M::PrimaryKey>,
    InnerKey: crate::ForeignKeyValue<R::PrimaryKey>,
{
    pub async fn all(
        self,
        db: &Database,
    ) -> Result<
        Vec<Loaded<M, (LoadedMany<Loaded<R, (LoadedMany<C, ChildRelation>,)>, Relation>,)>>,
        OrmError,
    > {
        db.transaction(move |tx| {
            Box::pin(async move {
                let parents = tx.fetch_all(self.query).await?;
                let field = self.outer.field();
                let children = tx
                    .fetch_by_many(
                        ModelField::<R, M::PrimaryKey>::new(field.table, field.name),
                        ids(&parents),
                    )
                    .await?;
                let field = self.inner.field();
                let grandchildren = tx
                    .fetch_by_many(
                        ModelField::<C, R::PrimaryKey>::new(field.table, field.name),
                        ids(&children),
                    )
                    .await?;
                let mut grandchildren = group(&self.inner, grandchildren);
                let mut grouped = HashMap::<M::PrimaryKey, Vec<_>>::new();
                for model in children {
                    if let Some(owner) =
                        crate::ForeignKeyValue::owner_key(self.outer.foreign_key(&model))
                    {
                        let related = grandchildren
                            .remove(&model.primary_key_value())
                            .unwrap_or_default();
                        grouped.entry(owner).or_default().push(Loaded {
                            model,
                            relations: (LoadedMany {
                                related,
                                _relation: PhantomData,
                            },),
                        });
                    }
                }
                Ok(parents
                    .into_iter()
                    .map(|model| {
                        let key = model.primary_key_value();
                        Loaded {
                            model,
                            relations: (LoadedMany {
                                related: grouped.remove(&key).unwrap_or_default(),
                                _relation: PhantomData,
                            },),
                        }
                    })
                    .collect())
            })
        })
        .await
    }
}
