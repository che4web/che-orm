use std::marker::PhantomData;

use sqlx::{
    Sqlite,
    query::{Query, QueryScalar},
    sqlite::SqliteArguments,
};

use crate::{Error, Model, ModelField, Result, SqliteBackend, SqliteModel, SqliteValue};

pub trait QueryField<M> {
    fn db_name(&self) -> &str;
}

impl<M> QueryField<M> for ModelField<M> {
    fn db_name(&self) -> &str {
        ModelField::db_name(self)
    }
}

impl<M> QueryField<M> for &str {
    fn db_name(&self) -> &str {
        self
    }
}

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
    predicate: Option<Q<M>>,
    orderings: Vec<Ordering>,
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

enum QNode {
    Compare {
        field: String,
        operator: QueryOperator,
        value: SqliteValue,
    },
    In {
        field: String,
        values: Vec<SqliteValue>,
    },
    IsNull {
        field: String,
        negated: bool,
    },
    And(Box<QNode>, Box<QNode>),
    Or(Box<QNode>, Box<QNode>),
    Not(Box<QNode>),
}

pub struct Q<M> {
    node: QNode,
    _model: PhantomData<M>,
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
            predicate: None,
            orderings: Vec::new(),
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
        update_by_values::<M>(self.db, id, values).await
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
    pub fn eq<F, V>(self, field: F, value: V) -> Self
    where
        F: QueryField<M>,
        V: Into<SqliteValue>,
    {
        self.filter(Q::compare(field, QueryOperator::Eq, value))
    }

    pub fn contains<F, V>(self, field: F, value: V) -> Self
    where
        F: QueryField<M>,
        V: Into<SqliteValue>,
    {
        self.filter(Q::compare(field, QueryOperator::Contains, value))
    }

    pub fn gt<F, V>(self, field: F, value: V) -> Self
    where
        F: QueryField<M>,
        V: Into<SqliteValue>,
    {
        self.filter(Q::compare(field, QueryOperator::Gt, value))
    }

    pub fn gte<F, V>(self, field: F, value: V) -> Self
    where
        F: QueryField<M>,
        V: Into<SqliteValue>,
    {
        self.filter(Q::compare(field, QueryOperator::Gte, value))
    }

    pub fn lt<F, V>(self, field: F, value: V) -> Self
    where
        F: QueryField<M>,
        V: Into<SqliteValue>,
    {
        self.filter(Q::compare(field, QueryOperator::Lt, value))
    }

    pub fn lte<F, V>(self, field: F, value: V) -> Self
    where
        F: QueryField<M>,
        V: Into<SqliteValue>,
    {
        self.filter(Q::compare(field, QueryOperator::Lte, value))
    }

    pub fn filter<E>(mut self, expression: E) -> Self
    where
        E: Into<Q<M>>,
    {
        let expression = expression.into();
        self.predicate = Some(match self.predicate.take() {
            Some(existing) => existing.and(expression),
            None => expression,
        });
        self
    }

    pub fn order_by<F>(mut self, field: F) -> Self
    where
        F: QueryField<M>,
    {
        let field = field.db_name();
        let (descending, field) = field
            .strip_prefix('-')
            .map_or((false, field), |field| (true, field));
        self.orderings.push(Ordering {
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
        append_query_parts::<M>(&mut sql, self.predicate.as_ref(), &mut values)?;
        append_ordering::<M>(&mut sql, &self.orderings)?;
        append_pagination(&mut sql, self.limit, self.offset);

        let query = bind_values(sqlx::query(&sql), values);
        let rows = query.fetch_all(self.db.pool()).await?;
        rows.iter()
            .map(M::from_row)
            .collect::<sqlx::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub async fn first(self) -> Result<Option<M>> {
        let mut values = Vec::new();
        let mut sql = format!("SELECT * FROM {}", M::table_name());
        append_query_parts::<M>(&mut sql, self.predicate.as_ref(), &mut values)?;
        append_ordering::<M>(&mut sql, &self.orderings)?;
        append_pagination(&mut sql, Some(1), self.offset);
        let row = bind_values(sqlx::query(&sql), values)
            .fetch_optional(self.db.pool())
            .await?;
        row.map(|row| M::from_row(&row).map_err(Into::into))
            .transpose()
    }

    pub async fn count(self) -> Result<i64> {
        let mut values = Vec::new();
        let mut sql = format!("SELECT COUNT(*) FROM {}", M::table_name());
        append_query_parts::<M>(&mut sql, self.predicate.as_ref(), &mut values)?;
        Ok(bind_scalar_values(sqlx::query_scalar(&sql), values)
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn sum<F>(self, field: F) -> Result<Option<f64>>
    where
        F: QueryField<M>,
    {
        self.aggregate("SUM", field).await
    }

    pub async fn avg<F>(self, field: F) -> Result<Option<f64>>
    where
        F: QueryField<M>,
    {
        self.aggregate("AVG", field).await
    }

    pub async fn min<F>(self, field: F) -> Result<Option<f64>>
    where
        F: QueryField<M>,
    {
        self.aggregate("MIN", field).await
    }

    pub async fn max<F>(self, field: F) -> Result<Option<f64>>
    where
        F: QueryField<M>,
    {
        self.aggregate("MAX", field).await
    }

    async fn aggregate<F>(self, function: &str, field: F) -> Result<Option<f64>>
    where
        F: QueryField<M>,
    {
        let field = checked_numeric_field::<M>(field.db_name())?;
        let mut values = Vec::new();
        let mut sql = format!(
            "SELECT CAST({function}({field}) AS REAL) FROM {}",
            M::table_name()
        );
        append_query_parts::<M>(&mut sql, self.predicate.as_ref(), &mut values)?;
        Ok(bind_scalar_values(sqlx::query_scalar(&sql), values)
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn update_one_returning<I, F, V>(self, updates: I) -> Result<Option<M>>
    where
        I: IntoIterator<Item = (F, V)>,
        F: QueryField<M>,
        V: Into<SqliteValue>,
    {
        let values = checked_updates::<M, I, F, V>(updates)?;
        if values.is_empty() {
            return Err(Error::EmptyUpdate);
        }

        let mut bindings = values
            .iter()
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>();
        let assignments = values
            .iter()
            .enumerate()
            .map(|(index, (field, _))| format!("{field} = ?{}", index + 1))
            .collect::<Vec<_>>();
        let timestamp_fields = update_timestamp_fields::<M>();
        let mut assignments = assignments;
        assignments.extend(
            timestamp_fields
                .iter()
                .map(|field| format!("{field} = CURRENT_TIMESTAMP")),
        );

        let where_sql =
            append_where::<M>(self.predicate.as_ref(), &mut bindings, values.len() + 1)?;
        let pk = M::primary_key().ok_or(Error::MissingPrimaryKey)?;
        let ordering = if self.orderings.is_empty() {
            vec![Ordering {
                field: pk.db_name.to_string(),
                descending: false,
            }]
        } else {
            self.orderings
        };
        let order_sql = render_ordering::<M>(&ordering)?;
        let subquery = format!(
            "SELECT {pk} FROM {table}{where_sql} ORDER BY {order_sql} LIMIT 1",
            pk = pk.db_name,
            table = M::table_name(),
        );
        let sql = format!(
            "UPDATE {} SET {} WHERE {} = ({}) RETURNING *",
            M::table_name(),
            assignments.join(", "),
            pk.db_name,
            subquery,
        );
        let row = bind_values(sqlx::query(&sql), bindings)
            .fetch_optional(self.db.pool())
            .await?;
        row.map(|row| M::from_row(&row).map_err(Into::into))
            .transpose()
    }

    pub async fn claim_next_returning<I, F, V>(self, updates: I) -> Result<Option<M>>
    where
        I: IntoIterator<Item = (F, V)>,
        F: QueryField<M>,
        V: Into<SqliteValue>,
    {
        let values = checked_updates::<M, I, F, V>(updates)?;
        if values.is_empty() {
            return Err(Error::EmptyUpdate);
        }

        let pk = M::primary_key().ok_or(Error::MissingPrimaryKey)?;
        let mut bindings = values
            .iter()
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>();
        let assignments = values
            .iter()
            .enumerate()
            .map(|(index, (field, _))| format!("{field} = ?{}", index + 1))
            .collect::<Vec<_>>();
        let timestamp_fields = update_timestamp_fields::<M>();
        let mut assignments = assignments;
        assignments.extend(
            timestamp_fields
                .iter()
                .map(|field| format!("{field} = CURRENT_TIMESTAMP")),
        );

        let where_sql =
            append_where::<M>(self.predicate.as_ref(), &mut bindings, values.len() + 1)?;
        let ordering = if self.orderings.is_empty() {
            vec![Ordering {
                field: pk.db_name.to_string(),
                descending: false,
            }]
        } else {
            self.orderings
        };
        let order_sql = render_ordering::<M>(&ordering)?;
        let subquery = format!(
            "SELECT {pk} FROM {table}{where_sql} ORDER BY {order_sql} LIMIT 1",
            pk = pk.db_name,
            table = M::table_name(),
        );
        let sql = format!(
            "UPDATE {} SET {} WHERE {} = ({}) RETURNING *",
            M::table_name(),
            assignments.join(", "),
            pk.db_name,
            subquery,
        );
        let row = bind_values(sqlx::query(&sql), bindings)
            .fetch_optional(self.db.pool())
            .await?;
        row.map(|row| M::from_row(&row).map_err(Into::into))
            .transpose()
    }
}

impl<M> Q<M> {
    fn compare<F, V>(field: F, operator: QueryOperator, value: V) -> Self
    where
        F: QueryField<M>,
        V: Into<SqliteValue>,
    {
        Self {
            node: QNode::Compare {
                field: field.db_name().to_string(),
                operator,
                value: value.into(),
            },
            _model: PhantomData,
        }
    }

    pub fn and(self, other: Self) -> Self {
        Self::combine(QNode::And(Box::new(self.node), Box::new(other.node)))
    }

    pub fn or(self, other: Self) -> Self {
        Self::combine(QNode::Or(Box::new(self.node), Box::new(other.node)))
    }

    pub fn not(self) -> Self {
        Self::combine(QNode::Not(Box::new(self.node)))
    }

    fn combine(node: QNode) -> Self {
        Self {
            node,
            _model: PhantomData,
        }
    }
}

impl<M> ModelField<M> {
    pub fn eq<V>(self, value: V) -> Q<M>
    where
        V: Into<SqliteValue>,
    {
        Q::compare(self, QueryOperator::Eq, value)
    }

    pub fn contains<V>(self, value: V) -> Q<M>
    where
        V: Into<SqliteValue>,
    {
        Q::compare(self, QueryOperator::Contains, value)
    }

    pub fn gt<V>(self, value: V) -> Q<M>
    where
        V: Into<SqliteValue>,
    {
        Q::compare(self, QueryOperator::Gt, value)
    }

    pub fn gte<V>(self, value: V) -> Q<M>
    where
        V: Into<SqliteValue>,
    {
        Q::compare(self, QueryOperator::Gte, value)
    }

    pub fn lt<V>(self, value: V) -> Q<M>
    where
        V: Into<SqliteValue>,
    {
        Q::compare(self, QueryOperator::Lt, value)
    }

    pub fn lte<V>(self, value: V) -> Q<M>
    where
        V: Into<SqliteValue>,
    {
        Q::compare(self, QueryOperator::Lte, value)
    }

    pub fn in_values<I, V>(self, values: I) -> Q<M>
    where
        I: IntoIterator<Item = V>,
        V: Into<SqliteValue>,
    {
        Q::combine(QNode::In {
            field: self.db_name().to_string(),
            values: values.into_iter().map(Into::into).collect(),
        })
    }

    pub fn is_null(self) -> Q<M> {
        Q::combine(QNode::IsNull {
            field: self.db_name().to_string(),
            negated: false,
        })
    }

    pub fn is_not_null(self) -> Q<M> {
        Q::combine(QNode::IsNull {
            field: self.db_name().to_string(),
            negated: true,
        })
    }
}

fn append_query_parts<M: Model>(
    sql: &mut String,
    predicate: Option<&Q<M>>,
    values: &mut Vec<SqliteValue>,
) -> Result<()> {
    if let Some(predicate) = predicate {
        sql.push_str(" WHERE ");
        sql.push_str(&render_predicate::<M>(&predicate.node, values)?);
    }
    Ok(())
}

fn append_ordering<M: Model>(sql: &mut String, orderings: &[Ordering]) -> Result<()> {
    if !orderings.is_empty() {
        sql.push_str(" ORDER BY ");
        sql.push_str(&render_ordering::<M>(orderings)?);
    }
    Ok(())
}

fn append_pagination(sql: &mut String, limit: Option<u32>, offset: Option<u32>) {
    match (limit, offset) {
        (Some(limit), Some(offset)) => sql.push_str(&format!(" LIMIT {limit} OFFSET {offset}")),
        (Some(limit), None) => sql.push_str(&format!(" LIMIT {limit}")),
        (None, Some(offset)) => sql.push_str(&format!(" LIMIT -1 OFFSET {offset}")),
        (None, None) => {}
    }
}

fn render_ordering<M: Model>(orderings: &[Ordering]) -> Result<String> {
    orderings
        .iter()
        .map(|ordering| {
            let field = checked_field::<M>(&ordering.field)?;
            let direction = if ordering.descending { "DESC" } else { "ASC" };
            Ok(format!("{field} {direction}"))
        })
        .collect::<Result<Vec<_>>>()
        .map(|parts| parts.join(", "))
}

fn render_predicate<M: Model>(node: &QNode, values: &mut Vec<SqliteValue>) -> Result<String> {
    match node {
        QNode::Compare {
            field,
            operator,
            value,
        } => {
            let field = checked_field::<M>(field)?;
            let placeholder = values.len() + 1;
            let operator = match operator {
                QueryOperator::Eq => "=",
                QueryOperator::Contains => "LIKE",
                QueryOperator::Gt => ">",
                QueryOperator::Gte => ">=",
                QueryOperator::Lt => "<",
                QueryOperator::Lte => "<=",
            };
            values.push(match operator {
                "LIKE" => contains_value(value.clone()),
                _ => value.clone(),
            });
            Ok(format!("{field} {operator} ?{placeholder}"))
        }
        QNode::In {
            field,
            values: in_values,
        } => {
            let field = checked_field::<M>(field)?;
            if in_values.is_empty() {
                return Ok("0".to_string());
            }
            let placeholders = in_values
                .iter()
                .map(|value| {
                    values.push(value.clone());
                    format!("?{}", values.len())
                })
                .collect::<Vec<_>>();
            Ok(format!("{field} IN ({})", placeholders.join(", ")))
        }
        QNode::IsNull { field, negated } => {
            let field = checked_field::<M>(field)?;
            Ok(format!(
                "{field} IS {}NULL",
                if *negated { "NOT " } else { "" }
            ))
        }
        QNode::And(left, right) => render_binary_predicate::<M>("AND", left, right, values),
        QNode::Or(left, right) => render_binary_predicate::<M>("OR", left, right, values),
        QNode::Not(node) => Ok(format!("NOT ({})", render_predicate::<M>(node, values)?)),
    }
}

fn render_binary_predicate<M: Model>(
    operator: &str,
    left: &QNode,
    right: &QNode,
    values: &mut Vec<SqliteValue>,
) -> Result<String> {
    Ok(format!(
        "({} {operator} {})",
        render_predicate::<M>(left, values)?,
        render_predicate::<M>(right, values)?
    ))
}

fn contains_value(value: SqliteValue) -> SqliteValue {
    match value {
        SqliteValue::String(value) => SqliteValue::String(format!("%{value}%")),
        value => value,
    }
}

fn checked_updates<M, I, F, V>(updates: I) -> Result<Vec<(&'static str, SqliteValue)>>
where
    M: Model,
    I: IntoIterator<Item = (F, V)>,
    F: QueryField<M>,
    V: Into<SqliteValue>,
{
    updates
        .into_iter()
        .map(|(field, value)| Ok((checked_update_field::<M>(field.db_name())?, value.into())))
        .collect()
}

fn append_where<M: Model>(
    predicate: Option<&Q<M>>,
    bindings: &mut Vec<SqliteValue>,
    start_index: usize,
) -> Result<String> {
    let Some(predicate) = predicate else {
        return Ok(String::new());
    };
    let mut local_bindings = Vec::new();
    let sql = render_predicate::<M>(&predicate.node, &mut local_bindings)?;
    bindings.extend(local_bindings);
    let sql = shift_placeholders(&sql, start_index);
    Ok(format!(" WHERE {sql}"))
}

fn shift_placeholders(sql: &str, start_index: usize) -> String {
    let mut result = String::new();
    let mut chars = sql.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '?' {
            let mut digits = String::new();
            while chars.peek().is_some_and(char::is_ascii_digit) {
                digits.push(chars.next().unwrap());
            }
            let index = digits.parse::<usize>().unwrap();
            result.push_str(&format!("?{}", index + start_index - 1));
        } else {
            result.push(ch);
        }
    }
    result
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
        let timestamp_fields = create_timestamp_fields::<M>();
        if self.values.is_empty() && timestamp_fields.is_empty() {
            let sql = format!("INSERT INTO {} DEFAULT VALUES RETURNING *", M::table_name());
            let row = sqlx::query(&sql).fetch_one(self.db.pool()).await?;
            return Ok(M::from_row(&row)?);
        }

        let mut values = Vec::with_capacity(self.values.len());
        for (field, value) in self.values {
            values.push((checked_create_field::<M>(&field)?, value));
        }

        let mut columns = values.iter().map(|(name, _)| *name).collect::<Vec<_>>();
        let mut placeholders = (1..=values.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>();
        for field in timestamp_fields {
            columns.push(field);
            placeholders.push("CURRENT_TIMESTAMP".to_string());
        }
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({}) RETURNING *",
            M::table_name(),
            columns.join(", "),
            placeholders.join(", ")
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
    let bind_count = values.len();
    let timestamp_fields = update_timestamp_fields::<M>();

    let mut assignments = values
        .iter()
        .enumerate()
        .map(|(index, (name, _))| format!("{name} = ?{}", index + 1))
        .collect::<Vec<_>>();
    for field in timestamp_fields {
        assignments.push(format!("{field} = CURRENT_TIMESTAMP"));
    }
    let id_placeholder = bind_count + 1;
    let sql = format!(
        "UPDATE {} SET {} WHERE {} = ?{} RETURNING *",
        M::table_name(),
        assignments.join(", "),
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

fn checked_numeric_field<M: Model>(field: &str) -> Result<&'static str> {
    let info = M::fields()
        .iter()
        .find(|info| info.db_name == field || info.rust_name == field)
        .ok_or_else(|| Error::UnknownField(field.to_string()))?;
    if !matches!(info.ty, crate::FieldType::Integer | crate::FieldType::Real) {
        return Err(Error::InvalidAggregateField(field.to_string()));
    }
    Ok(info.db_name)
}

fn checked_create_field<M: Model>(field: &str) -> Result<&'static str> {
    let info = M::fields()
        .iter()
        .find(|info| info.db_name == field || info.rust_name == field)
        .ok_or_else(|| Error::UnknownField(field.to_string()))?;

    if is_readonly_field(info) {
        return Err(Error::ReadonlyField(field.to_string()));
    }

    Ok(info.db_name)
}

fn checked_update_field<M: Model>(field: &str) -> Result<&'static str> {
    let info = M::fields()
        .iter()
        .find(|info| info.db_name == field || info.rust_name == field)
        .ok_or_else(|| Error::UnknownField(field.to_string()))?;

    if is_readonly_field(info) {
        return Err(Error::ReadonlyField(field.to_string()));
    }

    Ok(info.db_name)
}

fn is_readonly_field(info: &crate::FieldInfo) -> bool {
    info.primary_key || info.auto || info.auto_now_add || info.auto_now
}

fn create_timestamp_fields<M: Model>() -> Vec<&'static str> {
    M::fields()
        .iter()
        .filter(|field| field.auto_now_add || field.auto_now)
        .map(|field| field.db_name)
        .collect()
}

fn update_timestamp_fields<M: Model>() -> Vec<&'static str> {
    M::fields()
        .iter()
        .filter(|field| field.auto_now)
        .map(|field| field.db_name)
        .collect()
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
        SqliteValue::DateTime(value) => query.bind(value),
        SqliteValue::Json(value) => query.bind(value.to_string()),
        SqliteValue::Null => query.bind(Option::<i64>::None),
    })
}

fn bind_scalar_values<'q, T, I>(
    query: QueryScalar<'q, Sqlite, T, SqliteArguments<'q>>,
    values: I,
) -> QueryScalar<'q, Sqlite, T, SqliteArguments<'q>>
where
    I: IntoIterator<Item = SqliteValue>,
{
    values.into_iter().fold(query, |query, value| match value {
        SqliteValue::I64(value) => query.bind(value),
        SqliteValue::String(value) => query.bind(value),
        SqliteValue::Bool(value) => query.bind(value),
        SqliteValue::F64(value) => query.bind(value),
        SqliteValue::DateTime(value) => query.bind(value),
        SqliteValue::Json(value) => query.bind(value.to_string()),
        SqliteValue::Null => query.bind(Option::<i64>::None),
    })
}
