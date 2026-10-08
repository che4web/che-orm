use crate::{CompiledQuery, DatabaseValue, ExecuteResult, MigrationGraph, OrmError, Row};
use deadpool_postgres::{Manager, ManagerConfig, Object, RecyclingMethod, Runtime};
use std::sync::Arc;
use tokio_postgres::types::{ToSql, Type};

pub(crate) type Pool = deadpool_postgres::Pool;
pub(crate) const MAX_PARAMETERS: usize = 60_000;

pub(crate) async fn connect(url: String, size: usize) -> Result<Pool, OrmError> {
    if size == 0 {
        return Err(OrmError::Configuration("pool size must be positive".into()));
    }
    let config: tokio_postgres::Config = url
        .parse()
        .map_err(|error: tokio_postgres::Error| OrmError::Configuration(error.to_string()))?;
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|error| OrmError::Configuration(error.to_string()))?;
    let manager = Manager::from_config(
        config,
        postgres_native_tls::MakeTlsConnector::new(tls),
        ManagerConfig {
            recycling_method: RecyclingMethod::Verified,
        },
    );
    let pool = Pool::builder(manager)
        .max_size(size)
        .runtime(Runtime::Tokio1)
        .build()
        .map_err(|error| OrmError::Configuration(error.to_string()))?;
    Session::acquire(&pool).await?;
    Ok(pool)
}

#[derive(Clone)]
pub(crate) struct Session(Arc<Object>);

impl Session {
    pub(crate) async fn acquire(pool: &Pool) -> Result<Self, OrmError> {
        let object = tokio::time::timeout(std::time::Duration::from_secs(30), pool.get())
            .await
            .map_err(|_| OrmError::Pool("connection acquisition timed out".into()))?
            .map_err(|error| match error {
                deadpool_postgres::PoolError::Backend(error) => OrmError::Postgres(error),
                other => OrmError::Pool(other.to_string()),
            })?;
        let session = Self(Arc::new(object));
        session
            .batch("SET standard_conforming_strings = on")
            .await?;
        Ok(session)
    }
    pub(crate) async fn batch(&self, sql: &str) -> Result<(), OrmError> {
        self.0.batch_execute(sql).await?;
        Ok(())
    }
    pub(crate) async fn commit(&self) -> Result<(), OrmError> {
        // PostgreSQL reports COMMIT in an aborted transaction as a successful
        // ROLLBACK. The probe must succeed before COMMIT can be executed.
        self.batch("SELECT 1; COMMIT").await
    }
    pub(crate) async fn execute(&self, query: CompiledQuery) -> Result<ExecuteResult, OrmError> {
        let statement = self.0.prepare(&query.sql).await?;
        let params = params(&query, statement.params())?;
        let refs = params
            .iter()
            .map(|value| value.as_ref() as &(dyn ToSql + Sync))
            .collect::<Vec<_>>();
        let rows_affected = self.0.execute(&statement, &refs).await?;
        Ok(ExecuteResult {
            rows_affected: usize::try_from(rows_affected)
                .map_err(|error| OrmError::Interaction(error.to_string()))?,
            last_insert_rowid: None,
        })
    }
    pub(crate) async fn query(&self, query: CompiledQuery) -> Result<Vec<Row>, OrmError> {
        let statement = self.0.prepare(&query.sql).await?;
        let params = params(&query, statement.params())?;
        let refs = params
            .iter()
            .map(|value| value.as_ref() as &(dyn ToSql + Sync))
            .collect::<Vec<_>>();
        self.0
            .query(&statement, &refs)
            .await?
            .iter()
            .map(decode_row)
            .collect()
    }
    pub(crate) async fn migrate(
        &self,
        graph: MigrationGraph,
        target: Option<crate::MigrationId>,
    ) -> Result<usize, OrmError> {
        crate::migration::postgres_executor::apply(self, graph, target).await
    }
    pub(crate) async fn history(
        &self,
    ) -> Result<std::collections::HashMap<(String, String), String>, OrmError> {
        crate::migration::postgres_executor::history(self).await
    }
}

fn params(
    query: &CompiledQuery,
    types: &[Type],
) -> Result<Vec<Box<dyn ToSql + Sync + Send>>, OrmError> {
    if query.params.len() != types.len() {
        return Err(OrmError::Interaction("parameter count mismatch".into()));
    }
    query
        .params
        .iter()
        .zip(types)
        .map(|(value, ty)| {
            let value: Box<dyn ToSql + Sync + Send> = match value {
                DatabaseValue::Integer(value) => match *ty {
                    Type::INT2 => Box::new(i16::try_from(*value).map_err(|_| {
                        OrmError::Interaction(format!("integer parameter out of range for {ty}"))
                    })?),
                    Type::INT4 => Box::new(i32::try_from(*value).map_err(|_| {
                        OrmError::Interaction(format!("integer parameter out of range for {ty}"))
                    })?),
                    _ => Box::new(*value),
                },
                DatabaseValue::Real(value) if *ty == Type::FLOAT4 => {
                    let narrowed = *value as f32;
                    if value.is_finite() && !narrowed.is_finite() {
                        return Err(OrmError::Interaction(
                            "real parameter out of range for float4".into(),
                        ));
                    }
                    Box::new(narrowed)
                }
                DatabaseValue::Real(value) => Box::new(*value),
                DatabaseValue::Text(value) => Box::new(value.clone()),
                DatabaseValue::Binary(value) => Box::new(value.clone()),
                DatabaseValue::Boolean(value) => Box::new(*value),
                DatabaseValue::DateTime(value) => Box::new(*value),
                DatabaseValue::Uuid(value) => Box::new(*value),
                DatabaseValue::Null => match *ty {
                    Type::INT8 => Box::new(None::<i64>),
                    Type::INT4 => Box::new(None::<i32>),
                    Type::INT2 => Box::new(None::<i16>),
                    Type::FLOAT8 => Box::new(None::<f64>),
                    Type::FLOAT4 => Box::new(None::<f32>),
                    Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => {
                        Box::new(None::<String>)
                    }
                    Type::BYTEA => Box::new(None::<Vec<u8>>),
                    Type::BOOL => Box::new(None::<bool>),
                    Type::TIMESTAMPTZ => Box::new(None::<time::OffsetDateTime>),
                    Type::UUID => Box::new(None::<crate::Uuid>),
                    _ => {
                        return Err(OrmError::Interaction(format!(
                            "unsupported parameter type {ty}"
                        )));
                    }
                },
            };
            Ok(value)
        })
        .collect()
}

fn decode_row(row: &tokio_postgres::Row) -> Result<Row, OrmError> {
    let values = row
        .columns()
        .iter()
        .enumerate()
        .map(|(index, column)| {
            macro_rules! read {
                ($ty:ty, $variant:ident) => {
                    row.try_get::<_, Option<$ty>>(index)?
                        .map(DatabaseValue::$variant)
                        .unwrap_or(DatabaseValue::Null)
                };
            }
            Ok(match *column.type_() {
                Type::INT8 => read!(i64, Integer),
                Type::INT4 => row
                    .try_get::<_, Option<i32>>(index)?
                    .map(|v| DatabaseValue::Integer(i64::from(v)))
                    .unwrap_or(DatabaseValue::Null),
                Type::INT2 => row
                    .try_get::<_, Option<i16>>(index)?
                    .map(|v| DatabaseValue::Integer(i64::from(v)))
                    .unwrap_or(DatabaseValue::Null),
                Type::FLOAT8 => read!(f64, Real),
                Type::FLOAT4 => row
                    .try_get::<_, Option<f32>>(index)?
                    .map(|value| DatabaseValue::Real(f64::from(value)))
                    .unwrap_or(DatabaseValue::Null),
                Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => read!(String, Text),
                Type::BYTEA => read!(Vec<u8>, Binary),
                Type::BOOL => read!(bool, Boolean),
                Type::TIMESTAMPTZ => read!(time::OffsetDateTime, DateTime),
                Type::UUID => read!(crate::Uuid, Uuid),
                _ => {
                    return Err(OrmError::Interaction(format!(
                        "unsupported result type {}",
                        column.type_()
                    )));
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Row::new(values))
}
