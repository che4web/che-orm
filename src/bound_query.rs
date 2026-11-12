use crate::{
    BelongsTo, Database, DatabaseQuery, Expr, ForeignKeyValue, HasMany, Loaded, LoadedMany,
    LoadedOne, Model, MultiPrefetchRelatedQuery, MultiSelectRelatedQuery, NestedPrefetchQuery,
    OrderBy, OrmError, PrefetchRelatedQuery, PrefetchRelation, PrimaryKey, SelectQuery,
    SelectRelatedQuery, WithOne, WithOptionalOne,
};

/// A query bound to the database that created it.
///
/// Created by [`Database::query`]. Execution methods use that same database,
/// including its configured runtime. Standalone [`DatabaseQuery`] values remain
/// independent and accept an explicit database when executed.
pub struct BoundQuery<'db, Q> {
    database: &'db Database,
    query: Q,
}

impl<'db, Q> BoundQuery<'db, Q> {
    pub(crate) fn new(database: &'db Database, query: Q) -> Self {
        Self { database, query }
    }
}

macro_rules! filter_order {
    () => {
        pub fn filter(self, expr: Expr) -> Self {
            Self::new(self.database, self.query.filter(expr))
        }
        pub fn order_by(self, order: OrderBy) -> Self {
            Self::new(self.database, self.query.order_by(order))
        }
    };
}

macro_rules! pagination {
    () => {
        pub fn limit(self, limit: u64) -> Self {
            Self::new(self.database, self.query.limit(limit))
        }
        pub fn offset(self, offset: u64) -> Self {
            Self::new(self.database, self.query.offset(offset))
        }
    };
}

impl<'db, M: Model> BoundQuery<'db, DatabaseQuery<M>> {
    filter_order!();
    pagination!();

    pub fn select_related<R, Relation, Key>(
        self,
        relation: BelongsTo<M, R, Relation, Key>,
    ) -> BoundQuery<'db, SelectRelatedQuery<M, R, Relation, Key>> {
        BoundQuery::new(self.database, self.query.select_related(relation))
    }

    pub fn prefetch_related<P: PrefetchRelation<M>>(self, plan: P) -> BoundQuery<'db, P::Query> {
        BoundQuery::new(self.database, self.query.prefetch_related(plan))
    }

    pub async fn all(self) -> Result<Vec<M>, OrmError>
    where
        M: Send + 'static,
    {
        self.query.all(self.database).await
    }

    pub async fn first(self) -> Result<Option<M>, OrmError>
    where
        M: Send + 'static,
    {
        self.query.first(self.database).await
    }

    pub async fn count(self) -> Result<usize, OrmError>
    where
        M: Send + 'static,
    {
        self.query.count(self.database).await
    }

    pub fn into_select_query(self) -> SelectQuery<M> {
        self.query.into_select_query()
    }
}

impl<M: Model, R, Relation, Key> BoundQuery<'_, SelectRelatedQuery<M, R, Relation, Key>> {
    filter_order!();
    pagination!();

    pub async fn count(self) -> Result<usize, OrmError>
    where
        M: Send + 'static,
        R: Model,
        Key: ForeignKeyValue<R::PrimaryKey>,
    {
        self.query.count(self.database).await
    }
}

impl<'db, M, R, Relation, Key: PrimaryKey> BoundQuery<'db, SelectRelatedQuery<M, R, Relation, Key>>
where
    M: Model + Send + 'static,
    R: Model + Send + 'static,
{
    pub fn select_related<R2: Model, Relation2>(
        self,
        relation: BelongsTo<M, R2, Relation2, R2::PrimaryKey>,
    ) -> BoundQuery<'db, MultiSelectRelatedQuery<M, R, Relation, R2, Relation2, Key>> {
        BoundQuery::new(self.database, self.query.select_related(relation))
    }

    pub async fn all(self) -> Result<Vec<WithOne<M, R, Relation, Key>>, OrmError> {
        self.query.all(self.database).await
    }
}

impl<M, R, Relation, Key: PrimaryKey>
    BoundQuery<'_, SelectRelatedQuery<M, R, Relation, Option<Key>>>
where
    M: Model + Send + 'static,
    R: Model + Send + 'static,
{
    pub async fn all(self) -> Result<Vec<WithOptionalOne<M, R, Relation>>, OrmError> {
        self.query.all(self.database).await
    }
}

impl<M, R1, Relation1, R2, Relation2, Key1>
    BoundQuery<'_, MultiSelectRelatedQuery<M, R1, Relation1, R2, Relation2, Key1>>
where
    M: Model + Send + 'static,
    R1: Model + Send + 'static,
    R2: Model + Send + 'static,
{
    filter_order!();

    pub async fn all(
        self,
    ) -> Result<Vec<Loaded<M, (LoadedOne<R1, Relation1>, LoadedOne<R2, Relation2>)>>, OrmError>
    {
        self.query.all(self.database).await
    }
}

impl<'db, M, R, Relation, Key> BoundQuery<'db, PrefetchRelatedQuery<M, R, Relation, Key>>
where
    M: Model + Send + 'static,
    R: Model + Send + 'static,
    Key: ForeignKeyValue<M::PrimaryKey>,
{
    filter_order!();
    pagination!();

    pub fn prefetch_related<R2, Relation2, Key2: ForeignKeyValue<M::PrimaryKey>>(
        self,
        relation: HasMany<M, R2, Relation2, Key2>,
    ) -> BoundQuery<'db, MultiPrefetchRelatedQuery<M, R, Relation, R2, Relation2, Key, Key2>> {
        BoundQuery::new(self.database, self.query.prefetch_related(relation))
    }

    pub async fn count(self) -> Result<usize, OrmError> {
        self.query.count(self.database).await
    }

    pub async fn all(self) -> Result<Vec<Loaded<M, (LoadedMany<R, Relation>,)>>, OrmError>
    where
        Relation: Send + 'static,
    {
        self.query.all(self.database).await
    }
}

impl<M, R1, Relation1, R2, Relation2, Key1, Key2>
    BoundQuery<'_, MultiPrefetchRelatedQuery<M, R1, Relation1, R2, Relation2, Key1, Key2>>
where
    M: Model + Send + 'static,
    R1: Model + Send + 'static,
    R2: Model + Send + 'static,
    Relation1: Send + 'static,
    Relation2: Send + 'static,
    Key1: ForeignKeyValue<M::PrimaryKey>,
    Key2: ForeignKeyValue<M::PrimaryKey>,
{
    pub async fn all(
        self,
    ) -> Result<Vec<Loaded<M, (LoadedMany<R1, Relation1>, LoadedMany<R2, Relation2>)>>, OrmError>
    {
        self.query.all(self.database).await
    }
}

impl<M, R, Relation, C, ChildRelation, OuterKey, InnerKey>
    BoundQuery<'_, NestedPrefetchQuery<M, R, Relation, C, ChildRelation, OuterKey, InnerKey>>
where
    M: Model + Send + 'static,
    R: Model + Send + 'static,
    C: Model + Send + 'static,
    Relation: Send + 'static,
    ChildRelation: Send + 'static,
    OuterKey: ForeignKeyValue<M::PrimaryKey>,
    InnerKey: ForeignKeyValue<R::PrimaryKey>,
{
    pub async fn all(
        self,
    ) -> Result<
        Vec<Loaded<M, (LoadedMany<Loaded<R, (LoadedMany<C, ChildRelation>,)>, Relation>,)>>,
        OrmError,
    > {
        self.query.all(self.database).await
    }
}
