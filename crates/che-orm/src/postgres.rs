use std::path::Path;

use sqlx::{
    PgPool, Row,
    migrate::{Migrate, Migrator},
    postgres::PgPoolOptions,
};

use crate::query::{QNode, QueryOperator};
use crate::{
    DatabaseValue, FieldInfo, FieldType, MigrationStatus, ModelField, PostgresModel, Q, Result,
};

#[derive(Debug, Clone)]
pub struct PostgresBackend {
    pool: PgPool,
}

pub(crate) struct PostgresModelManager<'db, M> {
    db: &'db PostgresBackend,
    _model: std::marker::PhantomData<M>,
}

pub struct PostgresQueryBuilder<'db, M> {
    db: &'db PostgresBackend,
    predicate: Option<Q<M>>,
    orderings: Vec<(&'static str, bool)>,
    limit: Option<u32>,
    offset: Option<u32>,
    distinct: bool,
}

impl<'db, M: PostgresModel> PostgresModelManager<'db, M> {
    pub fn new(db: &'db PostgresBackend) -> Self {
        Self {
            db,
            _model: std::marker::PhantomData,
        }
    }

    pub async fn all(&self) -> Result<Vec<M>> {
        let sql = format!("SELECT * FROM {}", quote_identifier(M::table_name()));
        let rows = sqlx::query(&sql).fetch_all(self.db.pool()).await?;
        rows.iter()
            .map(|row| M::from_postgres_row(row).map_err(Into::into))
            .collect()
    }

    pub async fn get(&self, id: i64) -> Result<Option<M>> {
        let primary_key = primary_key::<M>()?;
        let sql = format!(
            "SELECT * FROM {} WHERE {} = $1 LIMIT 1",
            quote_identifier(M::table_name()),
            quote_identifier(primary_key.db_name)
        );
        sqlx::query(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?
            .map(|row| M::from_postgres_row(&row).map_err(Into::into))
            .transpose()
    }

    pub async fn create_values(&self, raw_values: Vec<(String, DatabaseValue)>) -> Result<M> {
        let mut values: Vec<(&'static FieldInfo, DatabaseValue)> =
            Vec::with_capacity(raw_values.len());
        for (name, value) in raw_values {
            let field = field_info::<M>(&name)?;
            if field.primary_key || field.auto || field.auto_now || field.auto_now_add {
                return Err(crate::Error::ReadonlyField(name));
            }
            values.push((field, value));
        }
        if values.is_empty() {
            let sql = format!(
                "INSERT INTO {} DEFAULT VALUES RETURNING *",
                quote_identifier(M::table_name())
            );
            return Ok(M::from_postgres_row(
                &sqlx::query(&sql).fetch_one(self.db.pool()).await?,
            )?);
        }
        let columns = values
            .iter()
            .map(|(field, _)| quote_identifier(field.db_name))
            .collect::<Vec<_>>()
            .join(", ");
        let placeholders = (1..=values.len())
            .map(|index| format!("${index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT INTO {} ({columns}) VALUES ({placeholders}) RETURNING *",
            quote_identifier(M::table_name())
        );
        let mut query = sqlx::query(&sql);
        for (field, value) in values {
            query = bind_value(query, value, field);
        }
        Ok(M::from_postgres_row(
            &query.fetch_one(self.db.pool()).await?,
        )?)
    }

    pub async fn save(&self, model: &M) -> Result<Option<M>> {
        self.update_values(
            model.id().into(),
            M::save_values(model)
                .into_iter()
                .map(|(field, value)| (field.to_string(), value))
                .collect(),
        )
        .await
    }

    pub(crate) async fn update_values(
        &self,
        id: i64,
        values: Vec<(String, DatabaseValue)>,
    ) -> Result<Option<M>> {
        if values.is_empty() {
            return Err(crate::Error::EmptyUpdate);
        }
        for (name, _) in &values {
            let field = field_info::<M>(name)?;
            if field.primary_key || field.auto || field.auto_now || field.auto_now_add {
                return Err(crate::Error::ReadonlyField(name.clone()));
            }
        }
        let assignments = values
            .iter()
            .enumerate()
            .map(|(index, (name, _))| format!("{} = ${}", quote_identifier(name), index + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let timestamp_fields = M::fields()
            .iter()
            .filter(|field| field.auto_now)
            .map(|field| format!("{} = CURRENT_TIMESTAMP", quote_identifier(field.db_name)))
            .collect::<Vec<_>>();
        let assignments = std::iter::once(assignments)
            .filter(|assignments| !assignments.is_empty())
            .chain(timestamp_fields)
            .collect::<Vec<_>>()
            .join(", ");
        let primary_key = primary_key::<M>()?;
        let sql = format!(
            "UPDATE {} SET {assignments} WHERE {} = ${} RETURNING *",
            quote_identifier(M::table_name()),
            quote_identifier(primary_key.db_name),
            values.len() + 1
        );
        let mut query = sqlx::query(&sql);
        for (name, value) in values {
            query = bind_value(query, value, field_info::<M>(&name)?);
        }
        let row = query.bind(id).fetch_optional(self.db.pool()).await?;
        row.map(|row| M::from_postgres_row(&row).map_err(Into::into))
            .transpose()
    }

    pub async fn delete(&self, id: i64) -> Result<bool> {
        let primary_key = primary_key::<M>()?;
        let sql = format!(
            "DELETE FROM {} WHERE {} = $1",
            quote_identifier(M::table_name()),
            quote_identifier(primary_key.db_name)
        );
        Ok(sqlx::query(&sql)
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected()
            != 0)
    }
}

impl<'db, M: PostgresModel> PostgresQueryBuilder<'db, M> {
    pub(crate) fn new(db: &'db PostgresBackend) -> Self {
        Self {
            db,
            predicate: None,
            orderings: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
        }
    }
    pub fn filter(mut self, expression: Q<M>) -> Self {
        self.predicate = Some(match self.predicate.take() {
            Some(existing) => existing.and(expression),
            None => expression,
        });
        self
    }

    pub fn order_by<T>(mut self, field: ModelField<M, T>) -> Self {
        self.orderings.push((field.db_name(), false));
        self
    }

    pub fn order_by_desc<T>(mut self, field: ModelField<M, T>) -> Self {
        self.orderings.push((field.db_name(), true));
        self
    }

    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }
    pub fn offset(mut self, offset: u32) -> Self {
        self.offset = Some(offset);
        self
    }
    pub fn distinct(mut self) -> Self {
        self.distinct = true;
        self
    }

    pub async fn all(self) -> Result<Vec<M>> {
        let (sql, values) = self.sql("*");
        let query = values
            .into_iter()
            .fold(sqlx::query(&sql), |query, (value, field)| {
                bind_value(query, value, field)
            });
        query
            .fetch_all(self.db.pool())
            .await?
            .iter()
            .map(|row| M::from_postgres_row(row).map_err(Into::into))
            .collect()
    }

    pub async fn first(mut self) -> Result<Option<M>> {
        self.limit = Some(1);
        let (sql, values) = self.sql("*");
        let query = values
            .into_iter()
            .fold(sqlx::query(&sql), |query, (value, field)| {
                bind_value(query, value, field)
            });
        query
            .fetch_optional(self.db.pool())
            .await?
            .map(|row| M::from_postgres_row(&row).map_err(Into::into))
            .transpose()
    }

    pub async fn count(mut self) -> Result<i64> {
        // Count the filtered relation, not the current result page.
        self.distinct = false;
        self.limit = None;
        self.offset = None;
        let (sql, values) = self.sql("COUNT(*)");
        let query = values
            .into_iter()
            .fold(sqlx::query(&sql), |query, (value, field)| {
                bind_value(query, value, field)
            });
        Ok(query.fetch_one(self.db.pool()).await?.try_get(0)?)
    }

    fn sql(&self, select: &str) -> (String, Vec<(DatabaseValue, &'static FieldInfo)>) {
        let prefix = if self.distinct {
            "SELECT DISTINCT"
        } else {
            "SELECT"
        };
        let mut sql = format!(
            "{prefix} {select} FROM {}",
            quote_identifier(M::table_name())
        );
        let mut values = Vec::new();
        if let Some(predicate) = &self.predicate {
            sql.push_str(" WHERE ");
            sql.push_str(&render_predicate::<M>(&predicate.node, &mut values));
        }
        if !self.orderings.is_empty() {
            sql.push_str(" ORDER BY ");
            sql.push_str(
                &self
                    .orderings
                    .iter()
                    .map(|(field, descending)| {
                        format!(
                            "{} {}",
                            quote_identifier(field),
                            if *descending { "DESC" } else { "ASC" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        match (self.limit, self.offset) {
            (Some(limit), Some(offset)) => sql.push_str(&format!(" LIMIT {limit} OFFSET {offset}")),
            (Some(limit), None) => sql.push_str(&format!(" LIMIT {limit}")),
            (None, Some(offset)) => sql.push_str(&format!(" OFFSET {offset}")),
            (None, None) => {}
        }
        (sql, values)
    }
}

fn render_predicate<M: PostgresModel>(
    node: &QNode,
    values: &mut Vec<(DatabaseValue, &'static FieldInfo)>,
) -> String {
    match node {
        QNode::Compare {
            field,
            operator,
            value,
        } => {
            let field =
                field_info::<M>(field).expect("typed fields are generated from model metadata");
            let value = match operator {
                QueryOperator::Contains => match value.clone() {
                    DatabaseValue::String(value) => DatabaseValue::String(format!("%{value}%")),
                    value => value,
                },
                _ => value.clone(),
            };
            values.push((value, field));
            let operator = match operator {
                QueryOperator::Eq => "=",
                QueryOperator::Contains => "LIKE",
                QueryOperator::Gt => ">",
                QueryOperator::Gte => ">=",
                QueryOperator::Lt => "<",
                QueryOperator::Lte => "<=",
            };
            format!(
                "{} {operator} ${}",
                quote_identifier(field.db_name),
                values.len()
            )
        }
        QNode::In {
            field,
            values: items,
        } => {
            let field =
                field_info::<M>(field).expect("typed fields are generated from model metadata");
            if items.is_empty() {
                return "FALSE".to_string();
            }
            let placeholders = items
                .iter()
                .map(|value| {
                    values.push((value.clone(), field));
                    format!("${}", values.len())
                })
                .collect::<Vec<_>>();
            format!(
                "{} IN ({})",
                quote_identifier(field.db_name),
                placeholders.join(", ")
            )
        }
        QNode::IsNull { field, negated } => format!(
            "{} IS {}NULL",
            quote_identifier(
                field_info::<M>(field)
                    .expect("typed fields are generated from model metadata")
                    .db_name
            ),
            if *negated { "NOT " } else { "" }
        ),
        QNode::And(left, right) => format!(
            "({} AND {})",
            render_predicate::<M>(left, values),
            render_predicate::<M>(right, values)
        ),
        QNode::Or(left, right) => format!(
            "({} OR {})",
            render_predicate::<M>(left, values),
            render_predicate::<M>(right, values)
        ),
        QNode::Not(node) => format!("NOT ({})", render_predicate::<M>(node, values)),
    }
}

fn field_info<M: PostgresModel>(name: &str) -> Result<&'static FieldInfo> {
    M::fields()
        .iter()
        .find(|field| field.db_name == name || field.rust_name == name)
        .ok_or_else(|| crate::Error::UnknownField(name.to_string()))
}

fn primary_key<M: PostgresModel>() -> Result<&'static FieldInfo> {
    M::primary_key().ok_or(crate::Error::MissingPrimaryKey)
}

fn bind_value<'q>(
    query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    value: DatabaseValue,
    field: &FieldInfo,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    match value {
        DatabaseValue::I64(value) => query.bind(value),
        DatabaseValue::String(value) => query.bind(value),
        DatabaseValue::Bool(value) => query.bind(value),
        DatabaseValue::F64(value) => query.bind(value),
        DatabaseValue::DateTime(value) => query.bind(value),
        DatabaseValue::Json(value) => query.bind(sqlx::types::Json(value)),
        DatabaseValue::Null => match field.ty {
            FieldType::Integer => query.bind(Option::<i64>::None),
            FieldType::Text | FieldType::Choice | FieldType::FilePath => {
                query.bind(Option::<String>::None)
            }
            FieldType::Boolean => query.bind(Option::<bool>::None),
            FieldType::Real => query.bind(Option::<f64>::None),
            FieldType::DateTime => query.bind(Option::<chrono::NaiveDateTime>::None),
            FieldType::Json => query.bind(Option::<sqlx::types::Json<serde_json::Value>>::None),
        },
    }
}

impl PostgresBackend {
    pub async fn connect(url: &str) -> Result<Self> {
        Ok(Self {
            pool: PgPoolOptions::new().connect(url).await?,
        })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn migrate(&self, migrations_dir: impl AsRef<Path>) -> Result<Vec<String>> {
        let migrator = Migrator::new(migrations_dir.as_ref()).await?;
        let applied_before: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success = TRUE")
                .fetch_all(&self.pool)
                .await
                .unwrap_or_default();
        migrator.run(&self.pool).await?;
        Ok(migrator
            .iter()
            .filter(|migration| !applied_before.contains(&migration.version))
            .map(|migration| migration.description.to_string())
            .collect())
    }

    pub async fn migration_status(
        &self,
        migrations_dir: impl AsRef<Path>,
    ) -> Result<Vec<MigrationStatus>> {
        let migrator = Migrator::new(migrations_dir.as_ref()).await?;
        let mut connection = self.pool.acquire().await?;
        connection.ensure_migrations_table().await?;
        let applied: Vec<(i64, bool, Vec<u8>)> = sqlx::query_as(
            "SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(migrator
            .iter()
            .map(|migration| {
                let stored = applied.iter().find(|row| row.0 == migration.version);
                MigrationStatus {
                    name: migration.description.to_string(),
                    applied: stored.is_some_and(|row| row.1),
                    checksum: stored
                        .map(|row| checksum_hex(&row.2))
                        .or_else(|| Some(checksum_hex(migration.checksum.as_ref()))),
                    checksum_mismatch: stored
                        .is_some_and(|row| row.2.as_slice() != migration.checksum.as_ref()),
                }
            })
            .collect())
    }
}

fn checksum_hex(checksum: &[u8]) -> String {
    checksum.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn quote_identifier(identifier: &str) -> String {
    // PostgreSQL historically received unquoted identifiers from che-orm and
    // folded them to lowercase. Preserve that behavior while quoting safely.
    format!(
        "\"{}\"",
        identifier.to_ascii_lowercase().replace('"', "\"\"")
    )
}
