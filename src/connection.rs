use std::collections::{HashMap, HashSet};
use std::{future::Future, pin::Pin};

use deadpool_sqlite::{Config, Pool, Runtime};
use rusqlite::{OptionalExtension, types::Value};
use time::format_description::well_known::Rfc3339;

use crate::{
    BelongsTo, CompiledQuery, DatabaseValue, Expr, HasMany, Model, ModelField, QueryAst,
    QueryBuildError, QueryValue, SelectQuery, SqlCompiler, SqliteDialect,
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

/// Bridge implemented by generated serializers for generic HTTP adapters.
pub trait ModelWriteSerializer {
    type Model: crate::Model;
    type CreateInput: serde::de::DeserializeOwned + Send;
    type UpdateInput: serde::de::DeserializeOwned + Send;
    type PatchInput: serde::de::DeserializeOwned + Send;

    fn create<'a>(
        database: &'a Database,
        input: Self::CreateInput,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Model, OrmError>> + Send + 'a>>;

    fn update<'a>(
        database: &'a Database,
        id: i64,
        input: Self::UpdateInput,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Self::Model>, OrmError>> + Send + 'a>>;

    fn patch<'a>(
        database: &'a Database,
        id: i64,
        input: Self::PatchInput,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Self::Model>, OrmError>> + Send + 'a>>;
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

    /// Fetches a model by its generated integer primary key.
    pub async fn get<M: Model + Send + 'static>(&self, id: i64) -> Result<Option<M>, OrmError> {
        self.fetch_one(M::query().filter(M::primary_key().eq(id)))
            .await
    }

    /// Fetches every row in a model's table.
    pub async fn all<M: Model + Send + 'static>(&self) -> Result<Vec<M>, OrmError> {
        self.fetch_all(M::query()).await
    }

    pub async fn count<M: Model + Send + 'static>(&self) -> Result<usize, OrmError> {
        let table = M::table_name();
        let pool = self.pool.clone();
        pool.get()
            .await?
            .interact(move |connection| {
                configure_connection(connection)?;
                connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
            })
            .await
            .map_err(|error| OrmError::Interaction(error.to_string()))?
            .map(|count| count as usize)
            .map_err(OrmError::from)
    }

    pub async fn count_query<M: Model + Send + 'static>(
        &self,
        query: SelectQuery<M>,
    ) -> Result<usize, OrmError> {
        let ast = query.into_select_ast().map_err(OrmError::QueryBuild)?;
        let compiled = SqlCompiler::<SqliteDialect>::compile(&QueryAst::Select(ast));
        let sql = format!("SELECT COUNT(*) FROM ({})", compiled.sql);
        let pool = self.pool.clone();
        pool.get()
            .await?
            .interact(move |connection| {
                configure_connection(connection)?;
                let params = sqlite_params(&compiled)?;
                connection.query_row(&sql, rusqlite::params_from_iter(params), |row| {
                    row.get::<_, i64>(0)
                })
            })
            .await
            .map_err(|error| OrmError::Interaction(error.to_string()))?
            .map(|count| count as usize)
            .map_err(OrmError::from)
    }

    /// Starts a high-level insert builder.
    pub fn create<M: Model>(&self) -> CreateBuilder<'_, M> {
        CreateBuilder {
            database: self,
            query: M::insert(),
        }
    }

    /// Starts a high-level update builder scoped to one primary key.
    pub fn update<M: Model>(&self, id: i64) -> UpdateBuilder<'_, M> {
        UpdateBuilder {
            database: self,
            query: M::update().filter(M::primary_key().eq(id)),
        }
    }

    /// Deletes one row by its generated integer primary key.
    pub async fn delete<M: Model + Send + 'static>(&self, id: i64) -> Result<bool, OrmError> {
        let result = self
            .execute(
                M::delete()
                    .filter(M::primary_key().eq(id))
                    .into_ast()
                    .map_err(OrmError::QueryBuild)?,
            )
            .await?;
        Ok(result.rows_affected == 1)
    }

    /// Starts a high-level typed select builder bound to this database.
    pub fn query<M: Model>(&self) -> DatabaseQuery<'_, M> {
        DatabaseQuery {
            database: self,
            query: M::query(),
        }
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

    /// Fetches rows whose typed field matches one of the supplied values.
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
        const SQLITE_SAFE_VARIABLE_LIMIT: usize = 900;
        let values = values
            .into_iter()
            .map(QueryValue::<T>::into_query_value)
            .collect::<Vec<_>>();
        if values.is_empty() {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        for chunk in values.chunks(SQLITE_SAFE_VARIABLE_LIMIT) {
            let filter = Expr::In {
                left: Box::new(Expr::Column(field.column())),
                values: chunk.to_vec(),
            };
            result.extend(self.fetch_all(M::query().filter(filter)).await?);
        }
        Ok(result)
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

    async fn execute_returning<M: Model + Send + 'static>(
        &self,
        ast: QueryAst,
    ) -> Result<Option<M>, OrmError> {
        ast.validate().map_err(OrmError::QueryBuild)?;
        let compiled = SqlCompiler::<SqliteDialect>::compile(&ast);
        let pool = self.pool.clone();
        pool.get()
            .await?
            .interact(move |connection| {
                configure_connection(connection)?;
                let mut statement = connection.prepare(&compiled.sql)?;
                let params = sqlite_params(&compiled)?;
                statement
                    .query_row(rusqlite::params_from_iter(params), M::from_row)
                    .optional()
            })
            .await
            .map_err(|error| OrmError::Interaction(error.to_string()))?
            .map_err(OrmError::from)
    }
}

/// High-level insert builder. The executed result is the complete inserted model.
pub struct CreateBuilder<'db, M> {
    database: &'db Database,
    query: crate::InsertQuery<M>,
}

impl<M: Model> CreateBuilder<'_, M> {
    pub fn set<T, V>(self, field: ModelField<M, T>, value: V) -> Self
    where
        V: QueryValue<T>,
    {
        Self {
            database: self.database,
            query: self.query.set(field, value),
        }
    }

    pub async fn execute(self) -> Result<M, OrmError>
    where
        M: Send + 'static,
    {
        let ast = self
            .query
            .returning_all()
            .into_ast()
            .map_err(OrmError::QueryBuild)?;
        self.database
            .execute_returning(ast)
            .await?
            .ok_or_else(|| OrmError::Interaction("insert did not return a row".into()))
    }
}

/// High-level update builder scoped to one model primary key.
pub struct UpdateBuilder<'db, M> {
    database: &'db Database,
    query: crate::UpdateQuery<M>,
}

impl<M: Model> UpdateBuilder<'_, M> {
    pub fn set<T, V>(self, field: ModelField<M, T>, value: V) -> Self
    where
        V: QueryValue<T>,
    {
        Self {
            database: self.database,
            query: self.query.set(field, value),
        }
    }

    pub async fn execute(self) -> Result<Option<M>, OrmError>
    where
        M: Send + 'static,
    {
        let ast = self
            .query
            .returning_all()
            .into_ast()
            .map_err(OrmError::QueryBuild)?;
        self.database.execute_returning(ast).await
    }
}

/// High-level typed select builder bound to a database.
pub struct DatabaseQuery<'db, M> {
    database: &'db Database,
    query: SelectQuery<M>,
}

impl<'db, M: Model> DatabaseQuery<'db, M> {
    pub fn filter(self, expr: Expr) -> Self {
        Self {
            database: self.database,
            query: self.query.filter(expr),
        }
    }

    pub fn order_by(self, order: crate::OrderBy) -> Self {
        Self {
            database: self.database,
            query: self.query.order_by(order),
        }
    }

    pub fn limit(self, limit: u64) -> Self {
        Self {
            database: self.database,
            query: self.query.limit(limit),
        }
    }

    pub fn offset(self, offset: u64) -> Self {
        Self {
            database: self.database,
            query: self.query.offset(offset),
        }
    }

    /// Loads one foreign-key relation without per-row queries.
    pub fn select_related<R, Relation, Key>(
        self,
        relation: BelongsTo<M, R, Relation, Key>,
    ) -> SelectRelatedQuery<'db, M, R, Relation, Key> {
        SelectRelatedQuery {
            database: self.database,
            query: self.query,
            relation,
        }
    }

    /// Loads a reverse foreign-key relation with one batched query.
    pub fn prefetch_related<P>(self, plan: P) -> P::Query<'db>
    where
        P: PrefetchRelation<M>,
    {
        plan.into_query(self.database, self.query)
    }

    pub async fn all(self) -> Result<Vec<M>, OrmError>
    where
        M: Send + 'static,
    {
        self.database.fetch_all(self.query).await
    }

    pub async fn first(self) -> Result<Option<M>, OrmError>
    where
        M: Send + 'static,
    {
        self.database.fetch_one(self.query).await
    }

    pub fn into_select_query(self) -> SelectQuery<M> {
        self.query
    }
}

pub trait PrefetchRelation<Owner: Model> {
    type Query<'db>
    where
        Owner: 'db;

    fn into_query<'db>(
        self,
        database: &'db Database,
        query: SelectQuery<Owner>,
    ) -> Self::Query<'db>
    where
        Owner: 'db;
}

/// A model and its eagerly loaded `belongs_to` relation.
#[derive(Debug)]
pub struct WithOne<M, R, Relation = (), Key = i64> {
    pub model: M,
    pub related: R,
    pub(crate) _relation: std::marker::PhantomData<Relation>,
    pub(crate) _key: std::marker::PhantomData<Key>,
}

#[derive(Debug)]
pub struct WithOptionalOne<M, R, Relation = ()> {
    pub model: M,
    pub related: Option<R>,
    pub(crate) _relation: std::marker::PhantomData<Relation>,
}

/// A model and its eagerly loaded reverse relation.
#[derive(Debug)]
pub struct WithMany<M, R, Relation = (), Key = i64> {
    pub model: M,
    pub related: Vec<R>,
    pub(crate) _relation: std::marker::PhantomData<Relation>,
    pub(crate) _key: std::marker::PhantomData<Key>,
}

/// A materialized model and a tuple of preloaded relations.
#[derive(Debug)]
pub struct Loaded<M, Relations> {
    pub model: M,
    pub relations: Relations,
}

/// One reverse relation inside a materialized relation tuple.
#[derive(Debug)]
pub struct LoadedMany<R, Relation = ()> {
    pub related: Vec<R>,
    pub(crate) _relation: std::marker::PhantomData<Relation>,
}

#[derive(Debug)]
pub struct LoadedOne<R, Relation = ()> {
    pub related: R,
    pub(crate) _relation: std::marker::PhantomData<Relation>,
}

/// Query which materializes a model together with one related model.
pub struct SelectRelatedQuery<'db, M, R, Relation = (), Key = i64> {
    database: &'db Database,
    query: SelectQuery<M>,
    relation: BelongsTo<M, R, Relation, Key>,
}

impl<'db, M, R, Relation> SelectRelatedQuery<'db, M, R, Relation, i64>
where
    M: Model + Send + 'static,
    R: Model + Send + 'static,
{
    pub fn filter(self, expr: Expr) -> Self {
        Self {
            database: self.database,
            query: self.query.filter(expr),
            relation: self.relation,
        }
    }

    pub fn order_by(self, order: crate::OrderBy) -> Self {
        Self {
            database: self.database,
            query: self.query.order_by(order),
            relation: self.relation,
        }
    }

    pub fn select_related<R2, Relation2>(
        self,
        relation: BelongsTo<M, R2, Relation2, i64>,
    ) -> MultiSelectRelatedQuery<'db, M, R, Relation, R2, Relation2> {
        MultiSelectRelatedQuery {
            database: self.database,
            query: self.query,
            first: self.relation,
            second: relation,
        }
    }

    pub async fn all(self) -> Result<Vec<WithOne<M, R, Relation>>, OrmError>
    where
        Relation: Send + 'static,
    {
        let ast = self
            .query
            .into_joined_ast(self.relation)
            .map_err(OrmError::QueryBuild)?;
        let compiled = SqlCompiler::<SqliteDialect>::compile(&ast);
        let pool = self.database.pool.clone();
        pool.get()
            .await?
            .interact(move |connection| {
                configure_connection(connection)?;
                load_joined_models::<M, R, Relation>(connection, &compiled)
            })
            .await
            .map_err(|error| OrmError::Interaction(error.to_string()))?
            .map_err(OrmError::from)
    }
}

pub struct MultiSelectRelatedQuery<'db, M, R1, Relation1, R2, Relation2> {
    database: &'db Database,
    query: SelectQuery<M>,
    first: BelongsTo<M, R1, Relation1, i64>,
    second: BelongsTo<M, R2, Relation2, i64>,
}

impl<M, R1, Relation1, R2, Relation2> MultiSelectRelatedQuery<'_, M, R1, Relation1, R2, Relation2>
where
    M: Model + Send + 'static,
    R1: Model + Send + 'static,
    R2: Model + Send + 'static,
    Relation1: Send + 'static,
    Relation2: Send + 'static,
{
    pub fn filter(self, expr: Expr) -> Self {
        Self {
            database: self.database,
            query: self.query.filter(expr),
            first: self.first,
            second: self.second,
        }
    }

    pub fn order_by(self, order: crate::OrderBy) -> Self {
        Self {
            database: self.database,
            query: self.query.order_by(order),
            first: self.first,
            second: self.second,
        }
    }

    pub async fn all(
        self,
    ) -> Result<Vec<Loaded<M, (LoadedOne<R1, Relation1>, LoadedOne<R2, Relation2>)>>, OrmError>
    {
        let first = self.first;
        let second = self.second;
        let mut ast = self
            .query
            .into_joined_ast(first)
            .map_err(OrmError::QueryBuild)?;
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
                    left: Box::new(crate::Expr::Column(second.field())),
                    op: crate::CompareOp::Eq,
                    right: Box::new(crate::Expr::Column(crate::ColumnRef::new(
                        second.alias(),
                        R2::primary_key().column().name,
                    ))),
                },
            });
        }
        let compiled = SqlCompiler::<SqliteDialect>::compile(&ast);
        let pool = self.database.pool.clone();
        pool.get()
            .await?
            .interact(move |connection| {
                configure_connection(connection)?;
                load_multi_joined_models::<M, R1, Relation1, R2, Relation2>(connection, &compiled)
            })
            .await
            .map_err(|error| OrmError::Interaction(error.to_string()))?
            .map_err(OrmError::from)
    }
}

impl<M, R, Relation> SelectRelatedQuery<'_, M, R, Relation, Option<i64>>
where
    M: Model + Send + 'static,
    R: Model + Send + 'static,
{
    pub fn filter(self, expr: Expr) -> Self {
        Self {
            database: self.database,
            query: self.query.filter(expr),
            relation: self.relation,
        }
    }

    pub fn order_by(self, order: crate::OrderBy) -> Self {
        Self {
            database: self.database,
            query: self.query.order_by(order),
            relation: self.relation,
        }
    }

    pub async fn all(self) -> Result<Vec<WithOptionalOne<M, R, Relation>>, OrmError>
    where
        Relation: Send + 'static,
    {
        let ast = self
            .query
            .into_optional_joined_ast(self.relation)
            .map_err(OrmError::QueryBuild)?;
        let compiled = SqlCompiler::<SqliteDialect>::compile(&ast);
        let pool = self.database.pool.clone();
        pool.get()
            .await?
            .interact(move |connection| {
                configure_connection(connection)?;
                load_optional_joined_models::<M, R, Relation>(connection, &compiled)
            })
            .await
            .map_err(|error| OrmError::Interaction(error.to_string()))?
            .map_err(OrmError::from)
    }
}

/// Query which materializes a model together with its reverse relation.
pub struct PrefetchRelatedQuery<'db, M, R, Relation = (), Key = i64> {
    database: &'db Database,
    query: SelectQuery<M>,
    relation: HasMany<M, R, Relation, Key>,
}

impl<M: Model, R, Relation> PrefetchRelation<M> for HasMany<M, R, Relation, i64> {
    type Query<'db>
        = PrefetchRelatedQuery<'db, M, R, Relation, i64>
    where
        M: 'db;

    fn into_query<'db>(self, database: &'db Database, query: SelectQuery<M>) -> Self::Query<'db>
    where
        M: 'db,
    {
        PrefetchRelatedQuery {
            database,
            query,
            relation: self,
        }
    }
}

pub struct NestedPrefetchPlan<M, R, Relation, C, ChildRelation> {
    outer: HasMany<M, R, Relation, i64>,
    inner: HasMany<R, C, ChildRelation, i64>,
}

impl<M, R, Relation> HasMany<M, R, Relation, i64> {
    pub fn prefetch<C, ChildRelation>(
        self,
        inner: HasMany<R, C, ChildRelation, i64>,
    ) -> NestedPrefetchPlan<M, R, Relation, C, ChildRelation> {
        NestedPrefetchPlan { outer: self, inner }
    }
}

impl<M: Model, R, Relation, C, ChildRelation> PrefetchRelation<M>
    for NestedPrefetchPlan<M, R, Relation, C, ChildRelation>
{
    type Query<'db>
        = NestedPrefetchQuery<'db, M, R, Relation, C, ChildRelation>
    where
        M: 'db;

    fn into_query<'db>(self, database: &'db Database, query: SelectQuery<M>) -> Self::Query<'db>
    where
        M: 'db,
    {
        NestedPrefetchQuery {
            database,
            query,
            outer: self.outer,
            inner: self.inner,
        }
    }
}

pub struct NestedPrefetchQuery<'db, M, R, Relation, C, ChildRelation> {
    database: &'db Database,
    query: SelectQuery<M>,
    outer: HasMany<M, R, Relation, i64>,
    inner: HasMany<R, C, ChildRelation, i64>,
}

impl<'db, M, R, Relation> PrefetchRelatedQuery<'db, M, R, Relation, i64>
where
    M: Model + Send + 'static,
    R: Model + Send + 'static,
{
    pub fn prefetch_related<R2, Relation2>(
        self,
        relation: HasMany<M, R2, Relation2>,
    ) -> MultiPrefetchRelatedQuery<'db, M, R, Relation, R2, Relation2> {
        MultiPrefetchRelatedQuery {
            database: self.database,
            query: self.query,
            first: self.relation,
            second: relation,
        }
    }

    pub async fn all(self) -> Result<Vec<Loaded<M, (LoadedMany<R, Relation>,)>>, OrmError>
    where
        Relation: Send + 'static,
    {
        let parent_ast = self.query.into_ast().map_err(OrmError::QueryBuild)?;
        let parent_compiled = SqlCompiler::<SqliteDialect>::compile(&parent_ast);
        let relation = self.relation;
        let pool = self.database.pool.clone();
        pool.get()
            .await?
            .interact(move |connection| {
                configure_connection(connection)?;
                connection.execute_batch("BEGIN")?;
                let result: rusqlite::Result<Vec<Loaded<M, (LoadedMany<R, Relation>,)>>> = (|| {
                    let parents = load_models::<M>(connection, &parent_compiled)?;
                    let parent_ids = parents
                        .iter()
                        .map(Model::primary_key_value)
                        .collect::<HashSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>();
                    let field = relation.field();
                    let related = load_models_by_ids(
                        connection,
                        ModelField::<R, i64>::new(field.table, field.name),
                        parent_ids,
                    )?;
                    let mut related_by_parent = HashMap::<i64, Vec<R>>::new();
                    for model in related {
                        related_by_parent
                            .entry(relation.foreign_key(&model))
                            .or_default()
                            .push(model);
                    }
                    Ok(parents
                        .into_iter()
                        .map(|model| {
                            let id = model.primary_key_value();
                            Loaded {
                                model,
                                relations: (LoadedMany {
                                    related: related_by_parent.remove(&id).unwrap_or_default(),
                                    _relation: std::marker::PhantomData,
                                },),
                            }
                        })
                        .collect())
                })(
                );
                match result {
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
}

/// Query which materializes two reverse relations in one typed result.
pub struct MultiPrefetchRelatedQuery<'db, M, R1, Relation1, R2, Relation2> {
    database: &'db Database,
    query: SelectQuery<M>,
    first: HasMany<M, R1, Relation1>,
    second: HasMany<M, R2, Relation2>,
}

impl<M, R, Relation, C, ChildRelation> NestedPrefetchQuery<'_, M, R, Relation, C, ChildRelation>
where
    M: Model + Send + 'static,
    R: Model + Send + 'static,
    C: Model + Send + 'static,
    Relation: Send + 'static,
    ChildRelation: Send + 'static,
{
    pub async fn all(
        self,
    ) -> Result<
        Vec<Loaded<M, (LoadedMany<Loaded<R, (LoadedMany<C, ChildRelation>,)>, Relation>,)>>,
        OrmError,
    > {
        let parents = self.database.fetch_all(self.query).await?;
        let parent_ids = parents
            .iter()
            .map(Model::primary_key_value)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let outer_field = self.outer.field();
        let children = self
            .database
            .fetch_by_many(
                ModelField::<R, i64>::new(outer_field.table, outer_field.name),
                parent_ids,
            )
            .await?;
        let child_ids = children
            .iter()
            .map(Model::primary_key_value)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let inner_field = self.inner.field();
        let grandchildren = self
            .database
            .fetch_by_many(
                ModelField::<C, i64>::new(inner_field.table, inner_field.name),
                child_ids,
            )
            .await?;
        let mut grandchildren_by_child = HashMap::<i64, Vec<C>>::new();
        for model in grandchildren {
            grandchildren_by_child
                .entry(self.inner.foreign_key(&model))
                .or_default()
                .push(model);
        }
        let children = children
            .into_iter()
            .map(|model| {
                let id = model.primary_key_value();
                Loaded {
                    model,
                    relations: (LoadedMany {
                        related: grandchildren_by_child.remove(&id).unwrap_or_default(),
                        _relation: std::marker::PhantomData,
                    },),
                }
            })
            .collect::<Vec<_>>();
        let mut children_by_parent = HashMap::<i64, Vec<_>>::new();
        for child in children {
            children_by_parent
                .entry(self.outer.foreign_key(&child.model))
                .or_default()
                .push(child);
        }
        Ok(parents
            .into_iter()
            .map(|model| {
                let id = model.primary_key_value();
                Loaded {
                    model,
                    relations: (LoadedMany {
                        related: children_by_parent.remove(&id).unwrap_or_default(),
                        _relation: std::marker::PhantomData,
                    },),
                }
            })
            .collect())
    }
}

impl<M, R1, Relation1, R2, Relation2> MultiPrefetchRelatedQuery<'_, M, R1, Relation1, R2, Relation2>
where
    M: Model + Send + 'static,
    R1: Model + Send + 'static,
    R2: Model + Send + 'static,
    Relation1: Send + 'static,
    Relation2: Send + 'static,
{
    pub async fn all(
        self,
    ) -> Result<Vec<Loaded<M, (LoadedMany<R1, Relation1>, LoadedMany<R2, Relation2>)>>, OrmError>
    {
        let parents = self.database.fetch_all(self.query).await?;
        let parent_ids = parents
            .iter()
            .map(Model::primary_key_value)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let first_field = self.first.field();
        let second_field = self.second.field();
        let first = self
            .database
            .fetch_by_many(
                ModelField::<R1, i64>::new(first_field.table, first_field.name),
                parent_ids.clone(),
            )
            .await?;
        let second = self
            .database
            .fetch_by_many(
                ModelField::<R2, i64>::new(second_field.table, second_field.name),
                parent_ids,
            )
            .await?;
        let mut first_by_parent = HashMap::<i64, Vec<R1>>::new();
        for model in first {
            first_by_parent
                .entry(self.first.foreign_key(&model))
                .or_default()
                .push(model);
        }
        let mut second_by_parent = HashMap::<i64, Vec<R2>>::new();
        for model in second {
            second_by_parent
                .entry(self.second.foreign_key(&model))
                .or_default()
                .push(model);
        }

        Ok(parents
            .into_iter()
            .map(|model| {
                let id = model.primary_key_value();
                Loaded {
                    model,
                    relations: (
                        LoadedMany {
                            related: first_by_parent.remove(&id).unwrap_or_default(),
                            _relation: std::marker::PhantomData,
                        },
                        LoadedMany {
                            related: second_by_parent.remove(&id).unwrap_or_default(),
                            _relation: std::marker::PhantomData,
                        },
                    ),
                }
            })
            .collect())
    }
}

fn load_joined_models<M: Model, R: Model, Relation>(
    connection: &rusqlite::Connection,
    compiled: &CompiledQuery,
) -> rusqlite::Result<Vec<WithOne<M, R, Relation>>> {
    let mut statement = connection.prepare(&compiled.sql)?;
    let params = sqlite_params(compiled)?;
    let offset = M::columns().len();
    let rows = statement.query_map(rusqlite::params_from_iter(params), |row| {
        Ok(WithOne {
            model: M::from_row_at(row, 0)?,
            related: R::from_row_at(row, offset)?,
            _relation: std::marker::PhantomData,
            _key: std::marker::PhantomData,
        })
    })?;
    rows.collect()
}

fn load_optional_joined_models<M: Model, R: Model, Relation>(
    connection: &rusqlite::Connection,
    compiled: &CompiledQuery,
) -> rusqlite::Result<Vec<WithOptionalOne<M, R, Relation>>> {
    let mut statement = connection.prepare(&compiled.sql)?;
    let params = sqlite_params(compiled)?;
    let offset = M::columns().len();
    let rows = statement.query_map(rusqlite::params_from_iter(params), |row| {
        let related_id: Option<i64> = row.get(offset)?;
        Ok(WithOptionalOne {
            model: M::from_row_at(row, 0)?,
            related: related_id
                .map(|_| R::from_row_at(row, offset))
                .transpose()?,
            _relation: std::marker::PhantomData,
        })
    })?;
    rows.collect()
}

fn load_multi_joined_models<M: Model, R1: Model, Relation1, R2: Model, Relation2>(
    connection: &rusqlite::Connection,
    compiled: &CompiledQuery,
) -> rusqlite::Result<Vec<Loaded<M, (LoadedOne<R1, Relation1>, LoadedOne<R2, Relation2>)>>> {
    let mut statement = connection.prepare(&compiled.sql)?;
    let params = sqlite_params(compiled)?;
    let first_offset = M::columns().len();
    let second_offset = first_offset + R1::columns().len();
    let rows = statement.query_map(rusqlite::params_from_iter(params), |row| {
        Ok(Loaded {
            model: M::from_row_at(row, 0)?,
            relations: (
                LoadedOne {
                    related: R1::from_row_at(row, first_offset)?,
                    _relation: std::marker::PhantomData,
                },
                LoadedOne {
                    related: R2::from_row_at(row, second_offset)?,
                    _relation: std::marker::PhantomData,
                },
            ),
        })
    })?;
    rows.collect()
}

fn load_models<M: Model>(
    connection: &rusqlite::Connection,
    compiled: &CompiledQuery,
) -> rusqlite::Result<Vec<M>> {
    let mut statement = connection.prepare(&compiled.sql)?;
    let params = sqlite_params(compiled)?;
    let rows = statement.query_map(rusqlite::params_from_iter(params), M::from_row)?;
    rows.collect()
}

fn load_models_by_ids<M: Model>(
    connection: &rusqlite::Connection,
    field: ModelField<M, i64>,
    ids: Vec<i64>,
) -> rusqlite::Result<Vec<M>> {
    const SQLITE_SAFE_VARIABLE_LIMIT: usize = 900;
    let mut result = Vec::new();
    for chunk in ids.chunks(SQLITE_SAFE_VARIABLE_LIMIT) {
        let ast = M::query()
            .filter(Expr::In {
                left: Box::new(Expr::Column(field.column())),
                values: chunk.iter().copied().map(DatabaseValue::Integer).collect(),
            })
            .into_ast()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        let compiled = SqlCompiler::<SqliteDialect>::compile(&ast);
        result.extend(load_models(connection, &compiled)?);
    }
    Ok(result)
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
