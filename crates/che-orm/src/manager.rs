use std::{any::TypeId, collections::HashMap, marker::PhantomData};

use sqlx::{
    Row, Sqlite,
    query::{Query, QueryScalar},
    sqlite::SqliteArguments,
};

use crate::{
    BelongsTo, Error, HasMany, Model, ModelField, QueryValue, Result, SqliteBackend, SqliteModel,
    SqliteValue,
    signals::{PostSaveEvent, PostUpdateEvent, snapshot},
};

const SQLITE_BIND_CHUNK_SIZE: usize = 900;

pub trait QueryField<M> {
    fn db_name(&self) -> &str;
}

impl<'db, M> ProjectionQuery<'db, M>
where
    M: SqliteModel,
{
    pub fn group_by(self) -> GroupQuery<'db, M> {
        GroupQuery {
            query: self.query,
            fields: self.fields,
            annotations: Vec::new(),
            having: None,
        }
    }

    pub fn distinct(mut self) -> Self {
        self.query.distinct = true;
        self
    }

    pub async fn all(self) -> Result<Vec<std::collections::HashMap<String, SqliteValue>>> {
        let fields = self.fields;
        let mut sql = String::from("SELECT ");
        if self.query.distinct {
            sql.push_str("DISTINCT ");
        }
        sql.push_str(
            &fields
                .iter()
                .map(|field| field.db_name)
                .collect::<Vec<_>>()
                .join(", "),
        );
        sql.push_str(" FROM ");
        sql.push_str(M::table_name());

        let mut values = Vec::new();
        append_query_parts::<M>(&mut sql, self.query.predicate.as_ref(), &mut values)?;
        append_ordering::<M>(&mut sql, &self.query.orderings)?;
        append_pagination(&mut sql, self.query.limit, self.query.offset);

        let rows = bind_values(sqlx::query(&sql), values)
            .fetch_all(self.query.db.pool())
            .await?;
        rows.iter().map(|row| project_row(row, &fields)).collect()
    }
}

impl<'db, M, S> TypedProjectionQuery<'db, M, S>
where
    M: SqliteModel,
    S: ProjectionSpec<M>,
{
    pub fn distinct(mut self) -> Self {
        self.query.distinct = true;
        self
    }

    pub async fn all(self) -> Result<Vec<S::Output>> {
        let fields = self.spec.fields()?;
        let select = fields
            .iter()
            .map(|field| field.db_name)
            .collect::<Vec<_>>()
            .join(", ");
        let prefix = if self.query.distinct {
            "SELECT DISTINCT"
        } else {
            "SELECT"
        };
        let mut sql = format!("{prefix} {select} FROM {}", M::table_name());
        let mut values = Vec::new();
        append_query_parts::<M>(&mut sql, self.query.predicate.as_ref(), &mut values)?;
        append_ordering::<M>(&mut sql, &self.query.orderings)?;
        append_pagination(&mut sql, self.query.limit, self.query.offset);
        let rows = bind_values(sqlx::query(&sql), values)
            .fetch_all(self.query.db.pool())
            .await?;
        rows.iter().map(|row| self.spec.decode(row)).collect()
    }
}

impl<'db, M> GroupQuery<'db, M>
where
    M: SqliteModel,
{
    pub fn having(mut self, expression: Q<M>) -> Self {
        self.having = Some(match self.having.take() {
            Some(existing) => existing.and(expression),
            None => expression,
        });
        self
    }

    pub fn having_annotation_field<T>(self, predicate: AnnotationPredicate<T>) -> Self {
        self.having_annotation(&predicate.alias, predicate.operator, predicate.value)
    }

    #[deprecated(note = "use AnnotationField::eq and having_annotation_field")]
    pub fn having_count<V>(self, alias: &str, value: V) -> Self
    where
        V: QueryValue<i64>,
    {
        self.having_annotation(alias, QueryOperator::Eq, value.into_query_value())
    }

    #[deprecated(note = "use AnnotationField::gte and having_annotation_field")]
    pub fn having_count_at_least<V>(self, alias: &str, value: V) -> Self
    where
        V: QueryValue<i64>,
    {
        self.having_annotation(alias, QueryOperator::Gte, value.into_query_value())
    }

    #[deprecated(note = "use AnnotationField::eq and having_annotation_field")]
    pub fn having_avg<V>(self, alias: &str, value: V) -> Self
    where
        V: QueryValue<f64>,
    {
        self.having_annotation(alias, QueryOperator::Eq, value.into_query_value())
    }

    #[deprecated(note = "use typed AnnotationField and having_annotation_field")]
    pub fn having_sum<F>(self, alias: &str, field: F, value: F::Value) -> Result<Self>
    where
        F: NumericQueryField<M>,
        F::Value: 'static,
        F::Value: QueryValue<F::Value>,
    {
        let field = resolve_field::<M, F>(field)?.db_name;
        let matches_sum = self.annotations.iter().any(|annotation| {
            matches!(annotation, Annotation::Sum { alias: candidate, field: source, .. } if candidate == alias && source.db_name == field)
        });
        if !matches_sum {
            return Err(Error::InvalidAnnotation(format!(
                "'{alias}' is not a SUM annotation for '{field}'"
            )));
        }
        Ok(self.having_annotation(alias, QueryOperator::Eq, value.into_query_value()))
    }

    fn having_annotation(
        mut self,
        alias: &str,
        operator: QueryOperator,
        value: SqliteValue,
    ) -> Self {
        let expression = Q::annotation_compare(alias, operator, value);
        self.having = Some(match self.having.take() {
            Some(existing) => existing.and(expression),
            None => expression,
        });
        self
    }

    pub fn annotate_count<F>(mut self, alias: &str, field: F) -> Result<Self>
    where
        F: TypedQueryField<M>,
    {
        self.validate_annotation_alias(alias)?;
        self.annotations.push(Annotation::Count {
            alias: alias.to_string(),
            field: resolve_field::<M, F>(field)?,
            type_id: TypeId::of::<i64>(),
        });
        Ok(self)
    }

    pub fn annotate_count_field<F>(
        self,
        annotation: &AnnotationField<i64>,
        field: F,
    ) -> Result<Self>
    where
        F: TypedQueryField<M>,
    {
        self.annotate_count(annotation.alias(), field)
    }

    pub fn annotate_avg_field<F>(self, annotation: &AnnotationField<f64>, field: F) -> Result<Self>
    where
        F: NumericQueryField<M>,
        F::Value: 'static,
    {
        self.annotate_avg(annotation.alias(), field)
    }

    pub fn annotate_sum_field<F>(
        self,
        annotation: &AnnotationField<F::Value>,
        field: F,
    ) -> Result<Self>
    where
        F: NumericQueryField<M>,
        F::Value: 'static,
    {
        self.annotate_sum(annotation.alias(), field)
    }

    pub fn annotate_min_field<F>(
        self,
        annotation: &AnnotationField<F::Value>,
        field: F,
    ) -> Result<Self>
    where
        F: NumericQueryField<M>,
        F::Value: 'static,
    {
        self.annotate_min(annotation.alias(), field)
    }

    pub fn annotate_max_field<F>(
        self,
        annotation: &AnnotationField<F::Value>,
        field: F,
    ) -> Result<Self>
    where
        F: NumericQueryField<M>,
        F::Value: 'static,
    {
        self.annotate_max(annotation.alias(), field)
    }

    pub fn annotate_sum<F>(mut self, alias: &str, field: F) -> Result<Self>
    where
        F: NumericQueryField<M>,
        F::Value: 'static,
    {
        self.validate_annotation_alias(alias)?;
        self.annotations.push(Annotation::Sum {
            alias: alias.to_string(),
            field: resolve_field::<M, F>(field)?,
            type_id: TypeId::of::<F::Value>(),
        });
        Ok(self)
    }

    pub fn annotate_avg<F>(mut self, alias: &str, field: F) -> Result<Self>
    where
        F: NumericQueryField<M>,
    {
        self.validate_annotation_alias(alias)?;
        self.annotations.push(Annotation::Avg {
            alias: alias.to_string(),
            field: resolve_field::<M, F>(field)?,
            type_id: TypeId::of::<f64>(),
        });
        Ok(self)
    }

    pub fn annotate_min<F>(mut self, alias: &str, field: F) -> Result<Self>
    where
        F: NumericQueryField<M>,
        F::Value: 'static,
    {
        self.validate_annotation_alias(alias)?;
        self.annotations.push(Annotation::Min {
            alias: alias.to_string(),
            field: resolve_field::<M, F>(field)?,
            type_id: TypeId::of::<F::Value>(),
        });
        Ok(self)
    }

    pub fn annotate_max<F>(mut self, alias: &str, field: F) -> Result<Self>
    where
        F: NumericQueryField<M>,
        F::Value: 'static,
    {
        self.validate_annotation_alias(alias)?;
        self.annotations.push(Annotation::Max {
            alias: alias.to_string(),
            field: resolve_field::<M, F>(field)?,
            type_id: TypeId::of::<F::Value>(),
        });
        Ok(self)
    }

    fn validate_annotation_alias(&self, alias: &str) -> Result<()> {
        if alias.is_empty() {
            return Err(Error::InvalidAnnotation(
                "alias cannot be empty".to_string(),
            ));
        }
        if self
            .annotations
            .iter()
            .any(|annotation| annotation_alias(annotation) == alias)
        {
            return Err(Error::InvalidAnnotation(format!(
                "duplicate alias '{alias}'"
            )));
        }
        if self.fields.iter().any(|field| field.db_name == alias) {
            return Err(Error::InvalidAnnotation(format!(
                "alias '{alias}' collides with a grouped field"
            )));
        }
        Ok(())
    }

    pub async fn all(self) -> Result<Vec<HashMap<String, SqliteValue>>> {
        if self.annotations.is_empty() {
            return Err(Error::UnsafeMigration(
                "grouped query requires at least one annotation".to_string(),
            ));
        }
        let mut select = self
            .fields
            .iter()
            .map(|field| field.db_name.to_string())
            .collect::<Vec<_>>();
        select.extend(self.annotations.iter().map(annotation_sql));
        let mut sql = format!("SELECT {} FROM {}", select.join(", "), M::table_name());
        let mut values = Vec::new();
        append_query_parts::<M>(&mut sql, self.query.predicate.as_ref(), &mut values)?;
        sql.push_str(" GROUP BY ");
        sql.push_str(
            &self
                .fields
                .iter()
                .map(|field| field.db_name)
                .collect::<Vec<_>>()
                .join(", "),
        );
        if let Some(having) = &self.having {
            let mut having_values = Vec::new();
            let having_sql = render_predicate::<M>(&having.node, &mut having_values)?;
            sql.push_str(" HAVING ");
            sql.push_str(&shift_placeholders(&having_sql, values.len() + 1));
            values.extend(having_values);
        }
        append_ordering::<M>(&mut sql, &self.query.orderings)?;
        append_pagination(&mut sql, self.query.limit, self.query.offset);

        let rows = bind_values(sqlx::query(&sql), values)
            .fetch_all(self.query.db.pool())
            .await?;
        rows.iter()
            .map(|row| {
                let mut result = project_row(row, &self.fields)?;
                for annotation in &self.annotations {
                    let (alias, field_type) = annotation_output(annotation);
                    result.insert(alias.to_string(), project_value(row, alias, field_type)?);
                }
                Ok(result)
            })
            .collect()
    }

    pub async fn all_typed<S>(self, spec: S) -> Result<Vec<S::Output>>
    where
        S: GroupProjectionSpec<M>,
    {
        if self.annotations.is_empty() {
            return Err(Error::UnsafeMigration(
                "grouped query requires at least one annotation".to_string(),
            ));
        }
        let mut expected_groups = self
            .fields
            .iter()
            .map(|field| field.db_name)
            .collect::<Vec<_>>();
        let mut actual_groups = spec.group_fields();
        expected_groups.sort_unstable();
        actual_groups.sort_unstable();
        if expected_groups != actual_groups {
            return Err(Error::InvalidAnnotation(
                "typed group spec does not match grouped fields".to_string(),
            ));
        }
        let expected_annotations = self
            .annotations
            .iter()
            .map(|annotation| (annotation_alias(annotation), annotation_type_id(annotation)))
            .collect::<Vec<_>>();
        let actual_annotations = spec.annotation_types();
        if expected_annotations.len() != actual_annotations.len()
            || expected_annotations
                .iter()
                .any(|expected| !actual_annotations.iter().any(|actual| actual == expected))
        {
            return Err(Error::InvalidAnnotation(
                "typed group spec does not match annotations".to_string(),
            ));
        }
        let mut select = self
            .fields
            .iter()
            .map(|field| field.db_name.to_string())
            .collect::<Vec<_>>();
        select.extend(self.annotations.iter().map(annotation_sql));
        let mut sql = format!("SELECT {} FROM {}", select.join(", "), M::table_name());
        let mut values = Vec::new();
        append_query_parts::<M>(&mut sql, self.query.predicate.as_ref(), &mut values)?;
        sql.push_str(" GROUP BY ");
        sql.push_str(
            &self
                .fields
                .iter()
                .map(|field| field.db_name)
                .collect::<Vec<_>>()
                .join(", "),
        );
        if let Some(having) = &self.having {
            let mut having_values = Vec::new();
            let having_sql = render_predicate::<M>(&having.node, &mut having_values)?;
            sql.push_str(" HAVING ");
            sql.push_str(&shift_placeholders(&having_sql, values.len() + 1));
            values.extend(having_values);
        }
        append_ordering::<M>(&mut sql, &self.query.orderings)?;
        append_pagination(&mut sql, self.query.limit, self.query.offset);
        let rows = bind_values(sqlx::query(&sql), values)
            .fetch_all(self.query.db.pool())
            .await?;
        rows.iter().map(|row| spec.decode(row)).collect()
    }
}

fn resolve_field<M, F>(field: F) -> Result<&'static crate::FieldInfo>
where
    M: Model,
    F: TypedQueryField<M>,
{
    M::fields()
        .iter()
        .find(|info| info.db_name == field.db_name())
        .ok_or_else(|| Error::UnknownField(field.db_name().to_string()))
}

fn annotation_alias(annotation: &Annotation) -> &str {
    match annotation {
        Annotation::Count { alias, .. }
        | Annotation::Sum { alias, .. }
        | Annotation::Avg { alias, .. }
        | Annotation::Min { alias, .. }
        | Annotation::Max { alias, .. } => alias,
    }
}

fn annotation_type_id(annotation: &Annotation) -> TypeId {
    match annotation {
        Annotation::Count { type_id, .. }
        | Annotation::Sum { type_id, .. }
        | Annotation::Avg { type_id, .. }
        | Annotation::Min { type_id, .. }
        | Annotation::Max { type_id, .. } => *type_id,
    }
}

fn annotation_sql(annotation: &Annotation) -> String {
    let (function, alias, field) = match annotation {
        Annotation::Count { alias, field, .. } => ("COUNT", alias, field),
        Annotation::Sum { alias, field, .. } => ("SUM", alias, field),
        Annotation::Avg { alias, field, .. } => ("AVG", alias, field),
        Annotation::Min { alias, field, .. } => ("MIN", alias, field),
        Annotation::Max { alias, field, .. } => ("MAX", alias, field),
    };
    format!(
        "{function}({field}) AS {}",
        quote_identifier(alias),
        field = field.db_name
    )
}

fn annotation_output(annotation: &Annotation) -> (&str, crate::FieldType) {
    match annotation {
        Annotation::Count { alias, .. } => (alias, crate::FieldType::Integer),
        Annotation::Sum { alias, field, .. }
        | Annotation::Min { alias, field, .. }
        | Annotation::Max { alias, field, .. } => (alias, field.ty),
        Annotation::Avg { alias, .. } => (alias, crate::FieldType::Real),
    }
}

fn project_row(
    row: &sqlx::sqlite::SqliteRow,
    fields: &[&'static crate::FieldInfo],
) -> Result<std::collections::HashMap<String, SqliteValue>> {
    fields
        .iter()
        .map(|field| {
            Ok((
                field.db_name.to_string(),
                project_value(row, field.db_name, field.ty)?,
            ))
        })
        .collect()
}

fn project_value(
    row: &sqlx::sqlite::SqliteRow,
    name: &str,
    field_type: crate::FieldType,
) -> Result<SqliteValue> {
    Ok(match field_type {
        crate::FieldType::Integer => row
            .try_get::<Option<i64>, _>(name)?
            .map_or(SqliteValue::Null, SqliteValue::I64),
        crate::FieldType::Boolean => row
            .try_get::<Option<bool>, _>(name)?
            .map_or(SqliteValue::Null, SqliteValue::Bool),
        crate::FieldType::Real => row
            .try_get::<Option<f64>, _>(name)?
            .map_or(SqliteValue::Null, SqliteValue::F64),
        crate::FieldType::DateTime => row
            .try_get::<Option<crate::NaiveDateTime>, _>(name)?
            .map_or(SqliteValue::Null, SqliteValue::DateTime),
        crate::FieldType::Json => match row.try_get::<Option<String>, _>(name)? {
            Some(value) => SqliteValue::Json(serde_json::from_str(&value)?),
            None => SqliteValue::Null,
        },
        crate::FieldType::Text | crate::FieldType::Choice | crate::FieldType::FilePath => row
            .try_get::<Option<String>, _>(name)?
            .map_or(SqliteValue::Null, SqliteValue::String),
    })
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

impl ProjectionValue for i64 {
    fn from_projection_row(row: &sqlx::sqlite::SqliteRow, field: &str) -> Result<Self> {
        Ok(row.try_get(field)?)
    }
}

impl ProjectionValue for i32 {
    fn from_projection_row(row: &sqlx::sqlite::SqliteRow, field: &str) -> Result<Self> {
        Ok(row.try_get(field)?)
    }
}

impl ProjectionValue for u32 {
    fn from_projection_row(row: &sqlx::sqlite::SqliteRow, field: &str) -> Result<Self> {
        Ok(row.try_get(field)?)
    }
}

impl ProjectionValue for bool {
    fn from_projection_row(row: &sqlx::sqlite::SqliteRow, field: &str) -> Result<Self> {
        Ok(row.try_get(field)?)
    }
}

impl ProjectionValue for f64 {
    fn from_projection_row(row: &sqlx::sqlite::SqliteRow, field: &str) -> Result<Self> {
        Ok(row.try_get(field)?)
    }
}

impl ProjectionValue for f32 {
    fn from_projection_row(row: &sqlx::sqlite::SqliteRow, field: &str) -> Result<Self> {
        Ok(row.try_get(field)?)
    }
}

impl ProjectionValue for String {
    fn from_projection_row(row: &sqlx::sqlite::SqliteRow, field: &str) -> Result<Self> {
        Ok(row.try_get(field)?)
    }
}

impl ProjectionValue for crate::NaiveDateTime {
    fn from_projection_row(row: &sqlx::sqlite::SqliteRow, field: &str) -> Result<Self> {
        Ok(row.try_get(field)?)
    }
}

impl ProjectionValue for crate::__private::serde_json::Value {
    fn from_projection_row(row: &sqlx::sqlite::SqliteRow, field: &str) -> Result<Self> {
        let value: String = row.try_get(field)?;
        Ok(serde_json::from_str(&value)?)
    }
}

macro_rules! optional_projection_value_impl {
    ($($type:ty),+ $(,)?) => {
        $(impl OptionalProjectionValue for $type {
            fn from_optional_projection_row(
                row: &sqlx::sqlite::SqliteRow,
                field: &str,
            ) -> Result<Option<Self>> {
                Ok(row.try_get(field)?)
            }
        })+
    };
}

optional_projection_value_impl!(i64, bool, f64, String, crate::NaiveDateTime);
optional_projection_value_impl!(i32, u32, f32);

impl OptionalProjectionValue for crate::__private::serde_json::Value {
    fn from_optional_projection_row(
        row: &sqlx::sqlite::SqliteRow,
        field: &str,
    ) -> Result<Option<Self>> {
        row.try_get::<Option<String>, _>(field)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(Into::into)
    }
}

impl ProjectionValue for crate::FilePath {
    fn from_projection_row(row: &sqlx::sqlite::SqliteRow, field: &str) -> Result<Self> {
        Ok(crate::FilePath::new(row.try_get::<String, _>(field)?)?)
    }
}

impl OptionalProjectionValue for crate::FilePath {
    fn from_optional_projection_row(
        row: &sqlx::sqlite::SqliteRow,
        field: &str,
    ) -> Result<Option<Self>> {
        row.try_get::<Option<String>, _>(field)?
            .map(crate::FilePath::new)
            .transpose()
    }
}

impl<M, T> ProjectionSpec<M> for ModelField<M, T>
where
    M: Model,
    T: ProjectionValue,
{
    type Output = T;

    fn fields(&self) -> Result<Vec<&'static crate::FieldInfo>> {
        Ok(vec![
            M::fields()
                .iter()
                .find(|info| info.db_name == self.db_name())
                .ok_or_else(|| Error::UnknownField(self.db_name().to_string()))?,
        ])
    }

    fn decode(&self, row: &sqlx::sqlite::SqliteRow) -> Result<Self::Output> {
        T::from_projection_row(row, self.db_name())
    }
}

impl<M, T> ProjectionSpec<M> for OptionalProjectionField<M, T>
where
    M: Model,
    T: OptionalProjectionValue,
{
    type Output = Option<T>;

    fn fields(&self) -> Result<Vec<&'static crate::FieldInfo>> {
        Ok(vec![
            M::fields()
                .iter()
                .find(|info| info.db_name == self.field)
                .ok_or_else(|| Error::UnknownField(self.field.to_string()))?,
        ])
    }

    fn decode(&self, row: &sqlx::sqlite::SqliteRow) -> Result<Self::Output> {
        T::from_optional_projection_row(row, self.field)
    }
}

impl<M, A, B> ProjectionSpec<M> for (ModelField<M, A>, ModelField<M, B>)
where
    M: Model,
    A: ProjectionValue,
    B: ProjectionValue,
{
    type Output = (A, B);

    fn fields(&self) -> Result<Vec<&'static crate::FieldInfo>> {
        let mut fields = self.0.fields()?;
        fields.extend(self.1.fields()?);
        Ok(fields)
    }

    fn decode(&self, row: &sqlx::sqlite::SqliteRow) -> Result<Self::Output> {
        Ok((self.0.decode(row)?, self.1.decode(row)?))
    }
}

impl<M, A, B, C> ProjectionSpec<M> for (ModelField<M, A>, ModelField<M, B>, ModelField<M, C>)
where
    M: Model,
    A: ProjectionValue,
    B: ProjectionValue,
    C: ProjectionValue,
{
    type Output = (A, B, C);

    fn fields(&self) -> Result<Vec<&'static crate::FieldInfo>> {
        let mut fields = self.0.fields()?;
        fields.extend(self.1.fields()?);
        fields.extend(self.2.fields()?);
        Ok(fields)
    }

    fn decode(&self, row: &sqlx::sqlite::SqliteRow) -> Result<Self::Output> {
        Ok((
            self.0.decode(row)?,
            self.1.decode(row)?,
            self.2.decode(row)?,
        ))
    }
}

impl<M, A> GroupProjectionSpec<M> for (AnnotationField<A>,)
where
    M: Model,
    A: ProjectionValue,
{
    type Output = (A,);

    fn group_fields(&self) -> Vec<&str> {
        Vec::new()
    }

    fn annotation_types(&self) -> Vec<(&str, TypeId)> {
        vec![(self.0.alias(), self.0.type_id())]
    }

    fn decode(&self, row: &sqlx::sqlite::SqliteRow) -> Result<Self::Output> {
        Ok((A::from_projection_row(row, self.0.alias())?,))
    }
}

impl<M, A, B> GroupProjectionSpec<M> for (ModelField<M, A>, AnnotationField<B>)
where
    M: Model,
    A: ProjectionValue,
    B: ProjectionValue,
{
    type Output = (A, B);

    fn group_fields(&self) -> Vec<&str> {
        vec![self.0.db_name()]
    }

    fn annotation_types(&self) -> Vec<(&str, TypeId)> {
        vec![(self.1.alias(), self.1.type_id())]
    }

    fn decode(&self, row: &sqlx::sqlite::SqliteRow) -> Result<Self::Output> {
        Ok((
            self.0.decode(row)?,
            B::from_projection_row(row, self.1.alias())?,
        ))
    }
}

impl<M, A, B, C> GroupProjectionSpec<M> for (ModelField<M, A>, ModelField<M, B>, AnnotationField<C>)
where
    M: Model,
    A: ProjectionValue,
    B: ProjectionValue,
    C: ProjectionValue,
{
    type Output = (A, B, C);

    fn group_fields(&self) -> Vec<&str> {
        vec![self.0.db_name(), self.1.db_name()]
    }

    fn annotation_types(&self) -> Vec<(&str, TypeId)> {
        vec![(self.2.alias(), self.2.type_id())]
    }

    fn decode(&self, row: &sqlx::sqlite::SqliteRow) -> Result<Self::Output> {
        Ok((
            self.0.decode(row)?,
            self.1.decode(row)?,
            C::from_projection_row(row, self.2.alias())?,
        ))
    }
}

impl<M, A, B, C> GroupProjectionSpec<M>
    for (ModelField<M, A>, AnnotationField<B>, AnnotationField<C>)
where
    M: Model,
    A: ProjectionValue,
    B: ProjectionValue,
    C: ProjectionValue,
{
    type Output = (A, B, C);

    fn group_fields(&self) -> Vec<&str> {
        vec![self.0.db_name()]
    }

    fn annotation_types(&self) -> Vec<(&str, TypeId)> {
        vec![
            (self.1.alias(), self.1.type_id()),
            (self.2.alias(), self.2.type_id()),
        ]
    }

    fn decode(&self, row: &sqlx::sqlite::SqliteRow) -> Result<Self::Output> {
        Ok((
            self.0.decode(row)?,
            B::from_projection_row(row, self.1.alias())?,
            C::from_projection_row(row, self.2.alias())?,
        ))
    }
}

impl<M, T> QueryField<M> for ModelField<M, T> {
    fn db_name(&self) -> &str {
        ModelField::db_name(self)
    }
}

pub trait TypedQueryField<M>: QueryField<M> {
    type Value;
}

impl<M, T> TypedQueryField<M> for ModelField<M, T> {
    type Value = T;
}

pub trait TextQueryField<M>: TypedQueryField<M, Value = String> {}

impl<M> TextQueryField<M> for ModelField<M, String> {}

pub trait NumericQueryField<M>: TypedQueryField<M> {}

impl<M> NumericQueryField<M> for ModelField<M, i64> {}
impl<M> NumericQueryField<M> for ModelField<M, i32> {}
impl<M> NumericQueryField<M> for ModelField<M, u32> {}
impl<M> NumericQueryField<M> for ModelField<M, f64> {}
impl<M> NumericQueryField<M> for ModelField<M, f32> {}

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
    distinct: bool,
    _model: PhantomData<M>,
}

pub struct ProjectionQuery<'db, M: Model> {
    query: QueryBuilder<'db, M>,
    fields: Vec<&'static crate::FieldInfo>,
}

pub struct TypedProjectionQuery<'db, M: Model, S> {
    query: QueryBuilder<'db, M>,
    spec: S,
}

pub trait ProjectionValue: Sized + 'static {
    fn from_projection_row(row: &sqlx::sqlite::SqliteRow, field: &str) -> Result<Self>;
}

pub trait OptionalProjectionValue: ProjectionValue {
    fn from_optional_projection_row(
        row: &sqlx::sqlite::SqliteRow,
        field: &str,
    ) -> Result<Option<Self>>;
}

pub trait ProjectionSpec<M: Model>: Sized {
    type Output;

    fn fields(&self) -> Result<Vec<&'static crate::FieldInfo>>;
    fn decode(&self, row: &sqlx::sqlite::SqliteRow) -> Result<Self::Output>;
}

pub trait GroupProjectionSpec<M: Model>: Sized {
    type Output;

    fn group_fields(&self) -> Vec<&str>;
    fn annotation_types(&self) -> Vec<(&str, TypeId)>;
    fn decode(&self, row: &sqlx::sqlite::SqliteRow) -> Result<Self::Output>;
}

pub struct OptionalProjectionField<M, T> {
    field: &'static str,
    _models: PhantomData<fn(M) -> T>,
}

pub struct GroupQuery<'db, M: Model> {
    query: QueryBuilder<'db, M>,
    fields: Vec<&'static crate::FieldInfo>,
    annotations: Vec<Annotation>,
    having: Option<Q<M>>,
}

#[derive(Debug, Clone)]
pub struct AnnotationField<T> {
    alias: String,
    _value: PhantomData<fn() -> T>,
}

pub struct AnnotationPredicate<T> {
    alias: String,
    operator: QueryOperator,
    value: SqliteValue,
    _value: PhantomData<fn() -> T>,
}

impl<T> AnnotationField<T> {
    pub fn new(alias: impl Into<String>) -> Self {
        Self {
            alias: alias.into(),
            _value: PhantomData,
        }
    }

    pub fn alias(&self) -> &str {
        &self.alias
    }

    pub fn type_id(&self) -> TypeId
    where
        T: 'static,
    {
        TypeId::of::<T>()
    }

    pub fn eq<V: QueryValue<T>>(self, value: V) -> AnnotationPredicate<T> {
        self.predicate(QueryOperator::Eq, value.into_query_value())
    }

    pub fn gt<V: QueryValue<T>>(self, value: V) -> AnnotationPredicate<T> {
        self.predicate(QueryOperator::Gt, value.into_query_value())
    }

    pub fn gte<V: QueryValue<T>>(self, value: V) -> AnnotationPredicate<T> {
        self.predicate(QueryOperator::Gte, value.into_query_value())
    }

    fn predicate(self, operator: QueryOperator, value: SqliteValue) -> AnnotationPredicate<T> {
        AnnotationPredicate {
            alias: self.alias,
            operator,
            value,
            _value: PhantomData,
        }
    }
}

enum Annotation {
    Count {
        alias: String,
        field: &'static crate::FieldInfo,
        type_id: TypeId,
    },
    Sum {
        alias: String,
        field: &'static crate::FieldInfo,
        type_id: TypeId,
    },
    Avg {
        alias: String,
        field: &'static crate::FieldInfo,
        type_id: TypeId,
    },
    Min {
        alias: String,
        field: &'static crate::FieldInfo,
        type_id: TypeId,
    },
    Max {
        alias: String,
        field: &'static crate::FieldInfo,
        type_id: TypeId,
    },
}

pub struct SelectRelatedQuery<'db, M: Model, R: Model> {
    query: QueryBuilder<'db, M>,
    relation: BelongsTo<M, R>,
}

pub struct PrefetchQuery<'db, M: Model, R: Model> {
    query: QueryBuilder<'db, M>,
    relation: HasMany<M, R>,
}

#[derive(Debug)]
pub struct Prefetched<M, R> {
    pub parents: Vec<M>,
    related: HashMap<String, Vec<R>>,
}

impl<M: Model, R> Prefetched<M, R> {
    pub fn related_for(&self, parent: &M) -> &[R] {
        let key = parent
            .get_value(
                M::primary_key()
                    .map(|field| field.db_name)
                    .unwrap_or_default(),
            )
            .map(|value| value.to_string())
            .unwrap_or_default();
        self.related.get(&key).map(Vec::as_slice).unwrap_or(&[])
    }
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
    AnnotationCompare {
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
            distinct: false,
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
        F: TypedQueryField<M>,
        V: QueryValue<F::Value>,
    {
        self.filter(Q::compare(field, QueryOperator::Eq, value))
    }

    pub fn eq_raw<V>(self, field: &str, value: V) -> Self
    where
        V: Into<SqliteValue>,
    {
        self.filter(Q::compare_raw(field, QueryOperator::Eq, value.into()))
    }

    pub fn in_raw<I, V>(self, field: &str, values: I) -> Self
    where
        I: IntoIterator<Item = V>,
        V: Into<SqliteValue>,
    {
        self.filter(Q::in_raw(
            field,
            values.into_iter().map(Into::into).collect(),
        ))
    }

    pub fn contains_raw<V>(self, field: &str, value: V) -> Self
    where
        V: Into<SqliteValue>,
    {
        self.filter(Q::compare_raw(field, QueryOperator::Contains, value.into()))
    }

    pub fn order_by_raw(mut self, field: &str, descending: bool) -> Self {
        self.orderings.push(Ordering {
            field: field.to_string(),
            descending,
        });
        self
    }

    pub fn contains<F, V>(self, field: F, value: V) -> Self
    where
        F: TextQueryField<M>,
        V: QueryValue<F::Value>,
    {
        self.filter(Q::compare(field, QueryOperator::Contains, value))
    }

    pub fn gt<F, V>(self, field: F, value: V) -> Self
    where
        F: TypedQueryField<M>,
        V: QueryValue<F::Value>,
    {
        self.filter(Q::compare(field, QueryOperator::Gt, value))
    }

    pub fn gte<F, V>(self, field: F, value: V) -> Self
    where
        F: TypedQueryField<M>,
        V: QueryValue<F::Value>,
    {
        self.filter(Q::compare(field, QueryOperator::Gte, value))
    }

    pub fn lt<F, V>(self, field: F, value: V) -> Self
    where
        F: TypedQueryField<M>,
        V: QueryValue<F::Value>,
    {
        self.filter(Q::compare(field, QueryOperator::Lt, value))
    }

    pub fn lte<F, V>(self, field: F, value: V) -> Self
    where
        F: TypedQueryField<M>,
        V: QueryValue<F::Value>,
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

    pub fn select_related<R>(self, relation: BelongsTo<M, R>) -> SelectRelatedQuery<'db, M, R>
    where
        R: Model,
    {
        SelectRelatedQuery {
            query: self,
            relation,
        }
    }

    pub fn prefetch_related<R>(self, relation: HasMany<M, R>) -> PrefetchQuery<'db, M, R>
    where
        R: Model,
    {
        PrefetchQuery {
            query: self,
            relation,
        }
    }

    pub fn distinct(mut self) -> Self {
        self.distinct = true;
        self
    }

    pub fn values<I, F>(self, fields: I) -> Result<ProjectionQuery<'db, M>>
    where
        I: IntoIterator<Item = F>,
        F: TypedQueryField<M>,
    {
        let fields = fields
            .into_iter()
            .map(|field| {
                M::fields()
                    .iter()
                    .find(|info| info.db_name == field.db_name())
                    .ok_or_else(|| Error::UnknownField(field.db_name().to_string()))
            })
            .collect::<Result<Vec<_>>>()?;
        if fields.is_empty() {
            return Err(Error::UnsafeMigration(
                "projection requires at least one field".to_string(),
            ));
        }
        Ok(ProjectionQuery {
            query: self,
            fields,
        })
    }

    pub fn select<S>(self, spec: S) -> Result<TypedProjectionQuery<'db, M, S>>
    where
        S: ProjectionSpec<M>,
    {
        spec.fields()?;
        Ok(TypedProjectionQuery { query: self, spec })
    }

    pub fn order_by<F>(mut self, field: F) -> Self
    where
        F: TypedQueryField<M>,
    {
        self.orderings.push(Ordering {
            field: field.db_name().to_string(),
            descending: false,
        });
        self
    }

    pub fn order_by_desc<F>(mut self, field: F) -> Self
    where
        F: TypedQueryField<M>,
    {
        self.orderings.push(Ordering {
            field: field.db_name().to_string(),
            descending: true,
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
        let select = if self.distinct {
            "SELECT DISTINCT *"
        } else {
            "SELECT *"
        };
        let mut sql = format!("{select} FROM {}", M::table_name());
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
        let select = if self.distinct {
            "SELECT DISTINCT *"
        } else {
            "SELECT *"
        };
        let mut sql = format!("{select} FROM {}", M::table_name());
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

    pub async fn sum<F>(self, field: F) -> Result<Option<F::Value>>
    where
        F: NumericQueryField<M>,
        F::Value: crate::AggregateValue,
    {
        self.aggregate_as("SUM", field).await
    }

    pub async fn avg<F>(self, field: F) -> Result<Option<f64>>
    where
        F: NumericQueryField<M>,
    {
        self.aggregate_as("AVG", field).await
    }

    pub async fn min<F>(self, field: F) -> Result<Option<F::Value>>
    where
        F: NumericQueryField<M>,
        F::Value: crate::AggregateValue,
    {
        self.aggregate_as("MIN", field).await
    }

    pub async fn max<F>(self, field: F) -> Result<Option<F::Value>>
    where
        F: NumericQueryField<M>,
        F::Value: crate::AggregateValue,
    {
        self.aggregate_as("MAX", field).await
    }

    async fn aggregate_as<T, F>(self, function: &str, field: F) -> Result<Option<T>>
    where
        F: NumericQueryField<M>,
        T: crate::AggregateValue,
    {
        let field = checked_numeric_field::<M>(field.db_name())?;
        let mut values = Vec::new();
        let mut sql = format!("SELECT {function}({field}) FROM {}", M::table_name());
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
        let Some(row) = row else {
            return Ok(None);
        };
        let model = M::from_row(&row)?;
        dispatch_post_update(self.db, &model);
        Ok(Some(model))
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
        let Some(row) = row else {
            return Ok(None);
        };
        let model = M::from_row(&row)?;
        dispatch_post_update(self.db, &model);
        Ok(Some(model))
    }
}

impl<'db, M, R> SelectRelatedQuery<'db, M, R>
where
    M: SqliteModel,
    R: SqliteModel + Clone,
{
    pub async fn all(self) -> Result<Vec<(M, Option<R>)>> {
        let db = self.query.db;
        let relation = self.relation;
        relation.validate()?;
        let parents = self.query.all().await?;
        let mut values = Vec::new();
        for parent in &parents {
            if let Some(value) = parent.get_value(relation.source_field()) {
                if let Some(value) = json_to_sqlite_value(value) {
                    values.push(value);
                }
            }
        }
        let mut related = HashMap::new();
        for chunk in values.chunks(SQLITE_BIND_CHUNK_SIZE) {
            for model in R::objects(db)
                .query()
                .in_raw("id", chunk.iter().cloned())
                .all()
                .await?
            {
                if let Some(value) = model.get_value("id") {
                    related.insert(value.to_string(), model);
                }
            }
        }
        Ok(parents
            .into_iter()
            .map(|parent| {
                let related = parent
                    .get_value(relation.source_field())
                    .and_then(|value| related.get(&value.to_string()).cloned());
                (parent, related)
            })
            .collect())
    }
}

impl<'db, M, R> PrefetchQuery<'db, M, R>
where
    M: SqliteModel,
    R: SqliteModel,
{
    pub async fn all(self) -> Result<Prefetched<M, R>> {
        let db = self.query.db;
        let relation = self.relation;
        relation.validate()?;
        let parents = self.query.all().await?;
        let mut parent_ids = Vec::new();
        for parent in &parents {
            if let Some(value) =
                parent.get_value(M::primary_key().ok_or(Error::MissingPrimaryKey)?.db_name)
            {
                if let Some(value) = json_to_sqlite_value(value) {
                    parent_ids.push(value);
                }
            }
        }
        let mut children = Vec::new();
        for chunk in parent_ids.chunks(SQLITE_BIND_CHUNK_SIZE) {
            children.extend(
                R::objects(db)
                    .query()
                    .in_raw(relation.child_field(), chunk.iter().cloned())
                    .all()
                    .await?,
            );
        }
        let mut related = HashMap::new();
        for child in children {
            if let Some(value) = child.get_value(relation.child_field()) {
                related
                    .entry(value.to_string())
                    .or_insert_with(Vec::new)
                    .push(child);
            }
        }
        Ok(Prefetched { parents, related })
    }
}

fn json_to_sqlite_value(value: crate::__private::serde_json::Value) -> Option<SqliteValue> {
    match value {
        crate::__private::serde_json::Value::Null => Some(SqliteValue::Null),
        crate::__private::serde_json::Value::Bool(value) => Some(SqliteValue::Bool(value)),
        crate::__private::serde_json::Value::String(value) => Some(SqliteValue::String(value)),
        crate::__private::serde_json::Value::Number(value) => value
            .as_i64()
            .map(SqliteValue::I64)
            .or_else(|| value.as_f64().map(SqliteValue::F64)),
        _ => None,
    }
}

impl<M> Q<M> {
    fn annotation_compare(field: &str, operator: QueryOperator, value: SqliteValue) -> Self {
        Self {
            node: QNode::AnnotationCompare {
                field: field.to_string(),
                operator,
                value,
            },
            _model: PhantomData,
        }
    }

    fn in_raw(field: &str, values: Vec<SqliteValue>) -> Self {
        Self {
            node: QNode::In {
                field: field.to_string(),
                values,
            },
            _model: PhantomData,
        }
    }

    fn compare_raw(field: &str, operator: QueryOperator, value: SqliteValue) -> Self {
        Self {
            node: QNode::Compare {
                field: field.to_string(),
                operator,
                value,
            },
            _model: PhantomData,
        }
    }

    fn compare<F, V>(field: F, operator: QueryOperator, value: V) -> Self
    where
        F: TypedQueryField<M>,
        V: QueryValue<F::Value>,
    {
        Self {
            node: QNode::Compare {
                field: field.db_name().to_string(),
                operator,
                value: value.into_query_value(),
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

impl<M, T> ModelField<M, T> {
    pub fn optional(self) -> OptionalProjectionField<M, T> {
        OptionalProjectionField {
            field: self.db_name(),
            _models: PhantomData,
        }
    }

    pub fn eq<V>(self, value: V) -> Q<M>
    where
        Self: TypedQueryField<M, Value = T>,
        V: QueryValue<T>,
    {
        Q::compare(self, QueryOperator::Eq, value)
    }

    pub fn gt<V>(self, value: V) -> Q<M>
    where
        V: QueryValue<T>,
    {
        Q::compare(self, QueryOperator::Gt, value)
    }

    pub fn gte<V>(self, value: V) -> Q<M>
    where
        V: QueryValue<T>,
    {
        Q::compare(self, QueryOperator::Gte, value)
    }

    pub fn lt<V>(self, value: V) -> Q<M>
    where
        V: QueryValue<T>,
    {
        Q::compare(self, QueryOperator::Lt, value)
    }

    pub fn lte<V>(self, value: V) -> Q<M>
    where
        V: QueryValue<T>,
    {
        Q::compare(self, QueryOperator::Lte, value)
    }

    pub fn in_values<I, V>(self, values: I) -> Q<M>
    where
        I: IntoIterator<Item = V>,
        Self: TypedQueryField<M, Value = T>,
        V: QueryValue<T>,
    {
        Q::combine(QNode::In {
            field: self.db_name().to_string(),
            values: values
                .into_iter()
                .map(QueryValue::into_query_value)
                .collect(),
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

impl<M> ModelField<M, String> {
    pub fn contains<V>(self, value: V) -> Q<M>
    where
        V: QueryValue<String>,
    {
        Q::compare(self, QueryOperator::Contains, value)
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
        QNode::AnnotationCompare {
            field,
            operator,
            value,
        } => {
            let placeholder = values.len() + 1;
            let operator = match operator {
                QueryOperator::Eq => "=",
                QueryOperator::Contains => "LIKE",
                QueryOperator::Gt => ">",
                QueryOperator::Gte => ">=",
                QueryOperator::Lt => "<",
                QueryOperator::Lte => "<=",
            };
            values.push(value.clone());
            Ok(format!(
                "{} {operator} ?{placeholder}",
                quote_identifier(field)
            ))
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
            let model = M::from_row(&row)?;
            dispatch_post_save(self.db, &model, true);
            return Ok(model);
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
        let model = M::from_row(&row)?;
        dispatch_post_save(self.db, &model, true);
        Ok(model)
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
    let model = M::from_row(&row)?;
    dispatch_post_update(db, &model);
    Ok(model)
}

fn dispatch_post_save<M: Model>(db: &SqliteBackend, model: &M, created: bool) {
    db.signals().dispatch_post_save::<M>(PostSaveEvent {
        table: M::table_name(),
        created,
        object: snapshot(model),
    });
}

fn dispatch_post_update<M: Model>(db: &SqliteBackend, model: &M) {
    let object = snapshot(model);
    db.signals().dispatch_post_save::<M>(PostSaveEvent {
        table: M::table_name(),
        created: false,
        object: object.clone(),
    });
    db.signals().dispatch_post_update::<M>(PostUpdateEvent {
        table: M::table_name(),
        object,
    });
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
