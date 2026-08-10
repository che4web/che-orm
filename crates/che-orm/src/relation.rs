use std::marker::PhantomData;

use crate::{Error, Model, Result, SqliteBackend, SqliteModel, SqliteValue};

#[derive(Debug, Clone, Copy)]
pub struct BelongsTo<From, To> {
    source_field: &'static str,
    _models: PhantomData<fn(From) -> To>,
}

impl<From, To> BelongsTo<From, To>
where
    From: SqliteModel,
    To: SqliteModel,
{
    pub const fn new(source_field: &'static str) -> Self {
        Self {
            source_field,
            _models: PhantomData,
        }
    }

    pub const fn source_field(&self) -> &'static str {
        self.source_field
    }

    pub const fn reverse(&self) -> HasMany<To, From> {
        HasMany::new(self.source_field)
    }

    pub fn validate(&self) -> Result<()> {
        let Some(field) = From::fields()
            .iter()
            .find(|field| field.db_name == self.source_field)
        else {
            return Err(Error::InvalidRelation(format!(
                "{}.{} does not exist",
                From::table_name(),
                self.source_field
            )));
        };
        if field
            .foreign_key
            .as_ref()
            .is_none_or(|foreign_key| foreign_key.table != To::table_name())
        {
            return Err(Error::InvalidRelation(format!(
                "{}.{} does not reference {}",
                From::table_name(),
                self.source_field,
                To::table_name()
            )));
        }
        Ok(())
    }

    pub async fn get<V>(&self, db: &SqliteBackend, value: V) -> Result<Option<To>>
    where
        V: Into<SqliteValue>,
    {
        self.validate()?;
        To::objects(db)
            .query()
            .eq_raw("id", value.into())
            .first()
            .await
    }

    pub async fn get_optional(&self, db: &SqliteBackend, value: Option<i64>) -> Result<Option<To>> {
        match value {
            Some(value) => self.get(db, value).await,
            None => Ok(None),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HasMany<Parent, Child> {
    child_field: &'static str,
    _models: PhantomData<fn(Parent) -> Child>,
}

impl<Parent, Child> HasMany<Parent, Child>
where
    Parent: Model,
    Child: SqliteModel,
{
    pub const fn new(child_field: &'static str) -> Self {
        Self {
            child_field,
            _models: PhantomData,
        }
    }

    pub const fn child_field(&self) -> &'static str {
        self.child_field
    }

    pub fn validate(&self) -> Result<()> {
        let Some(field) = Child::fields()
            .iter()
            .find(|field| field.db_name == self.child_field)
        else {
            return Err(Error::InvalidRelation(format!(
                "{}.{} does not exist",
                Child::table_name(),
                self.child_field
            )));
        };
        if field
            .foreign_key
            .as_ref()
            .is_none_or(|foreign_key| foreign_key.table != Parent::table_name())
        {
            return Err(Error::InvalidRelation(format!(
                "{}.{} does not reference {}",
                Child::table_name(),
                self.child_field,
                Parent::table_name()
            )));
        }
        Ok(())
    }

    pub fn query<'db, V>(
        &self,
        db: &'db SqliteBackend,
        parent_id: V,
    ) -> crate::QueryBuilder<'db, Child>
    where
        V: Into<SqliteValue>,
    {
        Child::objects(db)
            .query()
            .eq_raw(self.child_field, parent_id.into())
    }
}
