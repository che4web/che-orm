use std::marker::PhantomData;

use sqlx::{Sqlite, query::Query, sqlite::SqliteArguments};

use crate::{Error, Model, Result, SqliteBackend, SqliteModel, SqliteValue};

pub struct ModelManager<'db, M> {
    db: &'db SqliteBackend,
    _model: PhantomData<M>,
}

pub struct CreateBuilder<'db, M: Model> {
    db: &'db SqliteBackend,
    values: Vec<(String, SqliteValue)>,
    _model: PhantomData<M>,
}

pub struct UpdateBuilder<'db, M: Model> {
    db: &'db SqliteBackend,
    id: M::Id,
    values: Vec<(String, SqliteValue)>,
}

pub struct QueryBuilder<'db, M: Model> {
    db: &'db SqliteBackend,
    filters: Vec<QueryFilter>,
    ordering: Option<Ordering>,
    limit: Option<u32>,
    offset: Option<u32>,
    _model: PhantomData<M>,
}

#[derive(Debug, Clone, Copy)]
enum QueryOperator {
    Eq,
    Contains,
    Gt,
    Gte,
    Lt,
    Lte,
}

struct QueryFilter {
    field: String,
    operator: QueryOperator,
    value: SqliteValue,
}

struct Ordering {
    field: String,
    descending: bool,
}

impl<'db, M> ModelManager<'db, M>
where
    M: SqliteModel,
{
    pub fn new(db: &'db SqliteBackend) -> Self {
        Self {
            db,
            _model: PhantomData,
        }
    }

    pub fn create(&self) -> CreateBuilder<'db, M> {
        CreateBuilder {
            db: self.db,
            values: Vec::new(),
            _model: PhantomData,
        }
    }

    pub fn query(&self) -> QueryBuilder<'db, M> {
        QueryBuilder {
            db: self.db,
            filters: Vec::new(),
            ordering: None,
            limit: None,
            offset: None,
            _model: PhantomData,
        }
    }

    pub async fn get(&self, id: M::Id) -> Result<M> {
        let pk = M::primary_key().ok_or(Error::MissingPrimaryKey)?;
        let sql = format!(
            "SELECT * FROM {} WHERE {} = ?1 LIMIT 1",
            M::table_name(),
            pk.db_name
        );
        let row = sqlx::query(&sql).bind(id).fetch_one(self.db.pool()).await?;
        Ok(M::from_row(&row)?)
    }

    pub async fn all(&self) -> Result<Vec<M>> {
        let sql = format!("SELECT * FROM {}", M::table_name());
        let rows = sqlx::query(&sql).fetch_all(self.db.pool()).await?;
        rows.iter()
            .map(M::from_row)
            .collect::<sqlx::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub async fn filter_by_i64(&self, field: &str, value: i64) -> Result<Vec<M>> {
        let field = checked_field::<M>(field)?;
        let sql = format!("SELECT * FROM {} WHERE {} = ?1", M::table_name(), field);
        let rows = sqlx::query(&sql)
            .bind(value)
            .fetch_all(self.db.pool())
            .await?;
        rows.iter()
            .map(M::from_row)
            .collect::<sqlx::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub async fn first_by_i64(&self, field: &str, value: i64) -> Result<M> {
        let field = checked_field::<M>(field)?;
        let sql = format!(
            "SELECT * FROM {} WHERE {} = ?1 LIMIT 1",
            M::table_name(),
            field
        );
        let row = sqlx::query(&sql)
            .bind(value)
            .fetch_one(self.db.pool())
            .await?;
        Ok(M::from_row(&row)?)
    }

    pub async fn get_related<R>(&self, id: R::Id) -> Result<R>
    where
        R: SqliteModel,
    {
        ModelManager::<R>::new(self.db).get(id).await
    }

    pub async fn update(&self, id: M::Id, data: M::Update) -> Result<M> {
        let values = M::update_values(data);
        if values.is_empty() {
            return Err(Error::EmptyUpdate);
        }

        let pk = M::primary_key().ok_or(Error::MissingPrimaryKey)?;
        let assignments = values
            .iter()
            .enumerate()
            .map(|(index, (name, _))| format!("{name} = ?{}", index + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let id_placeholder = values.len() + 1;
        let sql = format!(
            "UPDATE {} SET {} WHERE {} = ?{} RETURNING *",
            M::table_name(),
            assignments,
            pk.db_name,
            id_placeholder
        );
        let query = bind_values(
            sqlx::query(&sql),
            values.into_iter().map(|(_, value)| value),
        )
        .bind(id);
        let row = query.fetch_one(self.db.pool()).await?;
        Ok(M::from_row(&row)?)
    }

    pub async fn save(&self, model: &M) -> Result<M> {
        let values = M::save_values(model);
        update_by_values::<M>(self.db, model.id(), values).await
    }

    pub fn update_fields(&self, id: M::Id) -> UpdateBuilder<'db, M> {
        UpdateBuilder {
            db: self.db,
            id,
            values: Vec::new(),
        }
    }

    pub async fn delete(&self, id: M::Id) -> Result<()> {
        let pk = M::primary_key().ok_or(Error::MissingPrimaryKey)?;
        let sql = format!("DELETE FROM {} WHERE {} = ?1", M::table_name(), pk.db_name);
        sqlx::query(&sql).bind(id).execute(self.db.pool()).await?;
        Ok(())
    }
}

impl<'db, M> QueryBuilder<'db, M>
where
    M: SqliteModel,
{
    pub fn eq<V>(self, field: &str, value: V) -> Self
    where
        V: Into<SqliteValue>,
    {
        self.filter(field, QueryOperator::Eq, value)
    }

    pub fn contains<V>(self, field: &str, value: V) -> Self
    where
        V: Into<SqliteValue>,
    {
        self.filter(field, QueryOperator::Contains, value)
    }

    pub fn gt<V>(self, field: &str, value: V) -> Self
    where
        V: Into<SqliteValue>,
    {
        self.filter(field, QueryOperator::Gt, value)
    }

    pub fn gte<V>(self, field: &str, value: V) -> Self
    where
        V: Into<SqliteValue>,
    {
        self.filter(field, QueryOperator::Gte, value)
    }

    pub fn lt<V>(self, field: &str, value: V) -> Self
    where
        V: Into<SqliteValue>,
    {
        self.filter(field, QueryOperator::Lt, value)
    }

    pub fn lte<V>(self, field: &str, value: V) -> Self
    where
        V: Into<SqliteValue>,
    {
        self.filter(field, QueryOperator::Lte, value)
    }

    pub fn order_by(mut self, field: &str) -> Self {
        let (descending, field) = field
            .strip_prefix('-')
            .map_or((false, field), |field| (true, field));
        self.ordering = Some(Ordering {
            field: field.to_string(),
            descending,
        });
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

    pub async fn all(self) -> Result<Vec<M>> {
        let mut values = Vec::new();
        let mut sql = format!("SELECT * FROM {}", M::table_name());

        if !self.filters.is_empty() {
            let mut clauses = Vec::new();
            for filter in self.filters {
                let field = checked_field::<M>(&filter.field)?;
                let placeholder = values.len() + 1;
                let operator = match filter.operator {
                    QueryOperator::Eq => "=",
                    QueryOperator::Contains => "LIKE",
                    QueryOperator::Gt => ">",
                    QueryOperator::Gte => ">=",
                    QueryOperator::Lt => "<",
                    QueryOperator::Lte => "<=",
                };
                clauses.push(format!("{field} {operator} ?{placeholder}"));
                values.push(match filter.operator {
                    QueryOperator::Contains => contains_value(filter.value),
                    _ => filter.value,
                });
            }
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }

        if let Some(ordering) = self.ordering {
            let field = checked_field::<M>(&ordering.field)?;
            let direction = if ordering.descending { "DESC" } else { "ASC" };
            sql.push_str(&format!(" ORDER BY {field} {direction}"));
        }

        match (self.limit, self.offset) {
            (Some(limit), Some(offset)) => {
                sql.push_str(&format!(" LIMIT {limit} OFFSET {offset}"));
            }
            (Some(limit), None) => {
                sql.push_str(&format!(" LIMIT {limit}"));
            }
            (None, Some(offset)) => {
                sql.push_str(&format!(" LIMIT -1 OFFSET {offset}"));
            }
            (None, None) => {}
        }

        let query = bind_values(sqlx::query(&sql), values);
        let rows = query.fetch_all(self.db.pool()).await?;
        rows.iter()
            .map(M::from_row)
            .collect::<sqlx::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn filter<V>(mut self, field: &str, operator: QueryOperator, value: V) -> Self
    where
        V: Into<SqliteValue>,
    {
        self.filters.push(QueryFilter {
            field: field.to_string(),
            operator,
            value: value.into(),
        });
        self
    }
}

fn contains_value(value: SqliteValue) -> SqliteValue {
    match value {
        SqliteValue::String(value) => SqliteValue::String(format!("%{value}%")),
        value => value,
    }
}

impl<'db, M> CreateBuilder<'db, M>
where
    M: SqliteModel,
{
    pub fn set<V>(mut self, field: &str, value: V) -> Self
    where
        V: Into<SqliteValue>,
    {
        self.values.push((field.to_string(), value.into()));
        self
    }

    pub fn set_null(mut self, field: &str) -> Self {
        self.values.push((field.to_string(), SqliteValue::Null));
        self
    }

    pub async fn execute(self) -> Result<M> {
        if self.values.is_empty() {
            let sql = format!("INSERT INTO {} DEFAULT VALUES RETURNING *", M::table_name());
            let row = sqlx::query(&sql).fetch_one(self.db.pool()).await?;
            return Ok(M::from_row(&row)?);
        }

        let mut values = Vec::with_capacity(self.values.len());
        for (field, value) in self.values {
            values.push((checked_create_field::<M>(&field)?, value));
        }

        let columns = values.iter().map(|(name, _)| *name).collect::<Vec<_>>();
        let placeholders = (1..=values.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({}) RETURNING *",
            M::table_name(),
            columns.join(", "),
            placeholders
        );
        let query = bind_values(
            sqlx::query(&sql),
            values.into_iter().map(|(_, value)| value),
        );
        let row = query.fetch_one(self.db.pool()).await?;
        Ok(M::from_row(&row)?)
    }
}

impl<'db, M> UpdateBuilder<'db, M>
where
    M: SqliteModel,
{
    pub fn set<V>(mut self, field: &str, value: V) -> Self
    where
        V: Into<SqliteValue>,
    {
        self.values.push((field.to_string(), value.into()));
        self
    }

    pub fn set_null(mut self, field: &str) -> Self {
        self.values.push((field.to_string(), SqliteValue::Null));
        self
    }

    pub async fn execute(self) -> Result<M> {
        if self.values.is_empty() {
            return Err(Error::EmptyUpdate);
        }

        let mut values = Vec::with_capacity(self.values.len());
        for (field, value) in self.values {
            values.push((checked_update_field::<M>(&field)?, value));
        }

        update_by_values::<M>(self.db, self.id, values).await
    }
}

async fn update_by_values<M>(
    db: &SqliteBackend,
    id: M::Id,
    values: Vec<(&'static str, SqliteValue)>,
) -> Result<M>
where
    M: SqliteModel,
{
    if values.is_empty() {
        return Err(Error::EmptyUpdate);
    }

    let pk = M::primary_key().ok_or(Error::MissingPrimaryKey)?;

    let assignments = values
        .iter()
        .enumerate()
        .map(|(index, (name, _))| format!("{name} = ?{}", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let id_placeholder = values.len() + 1;
    let sql = format!(
        "UPDATE {} SET {} WHERE {} = ?{} RETURNING *",
        M::table_name(),
        assignments,
        pk.db_name,
        id_placeholder
    );
    let query = bind_values(
        sqlx::query(&sql),
        values.into_iter().map(|(_, value)| value),
    )
    .bind(id);
    let row = query.fetch_one(db.pool()).await?;
    Ok(M::from_row(&row)?)
}

fn checked_field<M: Model>(field: &str) -> Result<&'static str> {
    M::fields()
        .iter()
        .find(|info| info.db_name == field || info.rust_name == field)
        .map(|info| info.db_name)
        .ok_or_else(|| Error::UnknownField(field.to_string()))
}

fn checked_create_field<M: Model>(field: &str) -> Result<&'static str> {
    let info = M::fields()
        .iter()
        .find(|info| info.db_name == field || info.rust_name == field)
        .ok_or_else(|| Error::UnknownField(field.to_string()))?;

    if info.primary_key || info.auto {
        return Err(Error::ReadonlyField(field.to_string()));
    }

    Ok(info.db_name)
}

fn checked_update_field<M: Model>(field: &str) -> Result<&'static str> {
    let info = M::fields()
        .iter()
        .find(|info| info.db_name == field || info.rust_name == field)
        .ok_or_else(|| Error::UnknownField(field.to_string()))?;

    if info.primary_key || info.auto {
        return Err(Error::ReadonlyField(field.to_string()));
    }

    Ok(info.db_name)
}

fn bind_values<'q, I>(
    query: Query<'q, Sqlite, SqliteArguments<'q>>,
    values: I,
) -> Query<'q, Sqlite, SqliteArguments<'q>>
where
    I: IntoIterator<Item = SqliteValue>,
{
    values.into_iter().fold(query, |query, value| match value {
        SqliteValue::I64(value) => query.bind(value),
        SqliteValue::String(value) => query.bind(value),
        SqliteValue::Bool(value) => query.bind(value),
        SqliteValue::F64(value) => query.bind(value),
        SqliteValue::Null => query.bind(Option::<i64>::None),
    })
}
