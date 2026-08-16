use std::{collections::HashMap, marker::PhantomData};

use che_orm2::{DatabaseQuery, Model, ModelField, QueryValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    Exact,
    Contains,
    Gt,
    Gte,
    Lt,
    Lte,
}

#[derive(Debug, thiserror::Error)]
pub enum FilterError {
    #[error("unknown filter: {0}")]
    UnknownFilter(String),
    #[error("unknown ordering field: {0}")]
    UnknownOrdering(String),
    #[error("invalid value for {field}, expected {expected}")]
    InvalidValue {
        field: String,
        expected: &'static str,
    },
}

pub trait FilterValue: QueryValue<Self> + Sized {
    const EXPECTED: &'static str;
    fn parse(value: &str) -> Result<Self, FilterError>;
}

macro_rules! parse_value {
    ($($ty:ty => $expected:literal),* $(,)?) => {
        $(
            impl FilterValue for $ty {
                const EXPECTED: &'static str = $expected;
                fn parse(value: &str) -> Result<Self, FilterError> {
                    value.parse().map_err(|_| FilterError::InvalidValue {
                        field: String::new(),
                        expected: Self::EXPECTED,
                    })
                }
            }
        )*
    };
}

parse_value!(i64 => "integer", bool => "boolean");

impl FilterValue for String {
    const EXPECTED: &'static str = "string";

    fn parse(value: &str) -> Result<Self, FilterError> {
        Ok(value.to_owned())
    }
}

type Apply<M> = for<'db> fn(
    DatabaseQuery<'db, M>,
    &'static str,
    &str,
) -> Result<DatabaseQuery<'db, M>, FilterError>;
type Order<M> = for<'db> fn(DatabaseQuery<'db, M>, &'static str, bool) -> DatabaseQuery<'db, M>;

#[derive(Clone, Copy)]
pub struct Filter<M: Model> {
    pub name: &'static str,
    pub source: &'static str,
    lookup: Lookup,
    apply: Apply<M>,
    order: Order<M>,
    _model: PhantomData<fn() -> M>,
}

impl<M: Model> Filter<M> {
    pub const fn exact<T>(field: ModelField<M, T>) -> Self
    where
        T: FilterValue,
    {
        Self::typed(field, Lookup::Exact, apply_exact::<M, T>)
    }

    pub const fn contains(field: ModelField<M, String>) -> Self {
        Self::typed(field, Lookup::Contains, apply_contains::<M>)
    }

    pub const fn gt<T>(field: ModelField<M, T>) -> Self
    where
        T: FilterValue,
    {
        Self::typed(field, Lookup::Gt, apply_gt::<M, T>)
    }

    pub const fn gte<T>(field: ModelField<M, T>) -> Self
    where
        T: FilterValue,
    {
        Self::typed(field, Lookup::Gte, apply_gte::<M, T>)
    }

    pub const fn lt<T>(field: ModelField<M, T>) -> Self
    where
        T: FilterValue,
    {
        Self::typed(field, Lookup::Lt, apply_lt::<M, T>)
    }

    pub const fn lte<T>(field: ModelField<M, T>) -> Self
    where
        T: FilterValue,
    {
        Self::typed(field, Lookup::Lte, apply_lte::<M, T>)
    }

    pub const fn exact_as<T>(name: &'static str, field: ModelField<M, T>) -> Self
    where
        T: FilterValue,
    {
        let mut filter = Self::exact(field);
        filter.name = name;
        filter
    }

    const fn typed<T>(field: ModelField<M, T>, lookup: Lookup, apply: Apply<M>) -> Self
    where
        T: FilterValue,
    {
        Self {
            name: field.column().name,
            source: field.column().name,
            lookup,
            apply,
            order: apply_order::<M, T>,
            _model: PhantomData,
        }
    }

    pub fn query_name(&self) -> String {
        match self.lookup {
            Lookup::Exact => self.name.to_owned(),
            Lookup::Contains => format!("{}__contains", self.name),
            Lookup::Gt => format!("{}__gt", self.name),
            Lookup::Gte => format!("{}__gte", self.name),
            Lookup::Lt => format!("{}__lt", self.name),
            Lookup::Lte => format!("{}__lte", self.name),
        }
    }

    fn matches(&self, name: &str) -> bool {
        match self.lookup {
            Lookup::Exact => name == self.name,
            Lookup::Contains => name == format!("{}__contains", self.name),
            Lookup::Gt => name == format!("{}__gt", self.name),
            Lookup::Gte => name == format!("{}__gte", self.name),
            Lookup::Lt => name == format!("{}__lt", self.name),
            Lookup::Lte => name == format!("{}__lte", self.name),
        }
    }
}

pub trait FilterSetSpec: Clone + Send + Sync + 'static {
    type Model: Model;
    fn filters(&self) -> &'static [Filter<Self::Model>];

    fn apply<'db>(
        &self,
        query: DatabaseQuery<'db, Self::Model>,
        params: &HashMap<String, String>,
    ) -> Result<DatabaseQuery<'db, Self::Model>, FilterError> {
        FilterSet::new(self.filters()).apply(query, params)
    }

    fn filterset(&self) -> FilterSet<Self::Model> {
        FilterSet::new(self.filters())
    }
}

pub struct FilterSet<M: Model + 'static> {
    filters: &'static [Filter<M>],
    _model: PhantomData<fn() -> M>,
}

impl<M: Model + 'static> Clone for FilterSet<M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M: Model + 'static> Copy for FilterSet<M> {}

impl<M: Model + 'static> Default for FilterSet<M> {
    fn default() -> Self {
        Self::new(&[])
    }
}

impl<M: Model + 'static> FilterSet<M> {
    pub const fn new(filters: &'static [Filter<M>]) -> Self {
        Self {
            filters,
            _model: PhantomData,
        }
    }

    pub fn apply<'db>(
        &self,
        mut query: DatabaseQuery<'db, M>,
        params: &HashMap<String, String>,
    ) -> Result<DatabaseQuery<'db, M>, FilterError> {
        for (name, value) in params {
            if matches!(name.as_str(), "limit" | "offset" | "ordering") {
                continue;
            }
            let filter = self
                .filters
                .iter()
                .find(|filter| filter.matches(name))
                .ok_or_else(|| FilterError::UnknownFilter(name.clone()))?;
            query = (filter.apply)(query, filter.source, value).map_err(|error| match error {
                FilterError::InvalidValue { expected, .. } => FilterError::InvalidValue {
                    field: name.clone(),
                    expected,
                },
                error => error,
            })?;
        }

        if let Some(ordering) = params.get("ordering") {
            let descending = ordering.starts_with('-');
            let name = ordering.strip_prefix('-').unwrap_or(ordering);
            let filter = self
                .filters
                .iter()
                .find(|filter| filter.name == name)
                .ok_or_else(|| FilterError::UnknownOrdering(name.to_owned()))?;
            query = (filter.order)(query, filter.source, descending);
        }
        Ok(query)
    }
}

impl<M: Model + 'static> FilterSetSpec for FilterSet<M> {
    type Model = M;
    fn filters(&self) -> &'static [Filter<M>] {
        self.filters
    }

    fn apply<'db>(
        &self,
        query: DatabaseQuery<'db, M>,
        params: &HashMap<String, String>,
    ) -> Result<DatabaseQuery<'db, M>, FilterError> {
        FilterSet::apply(self, query, params)
    }
}

fn apply_exact<'db, M: Model, T: FilterValue>(
    query: DatabaseQuery<'db, M>,
    field: &'static str,
    value: &str,
) -> Result<DatabaseQuery<'db, M>, FilterError> {
    let value = T::parse(value)?;
    Ok(query.filter(ModelField::<M, T>::new(M::table_name(), field).eq(value)))
}

fn apply_contains<'db, M: Model>(
    query: DatabaseQuery<'db, M>,
    field: &'static str,
    value: &str,
) -> Result<DatabaseQuery<'db, M>, FilterError> {
    Ok(query.filter(ModelField::<M, String>::new(M::table_name(), field).contains(value)))
}

macro_rules! range_filter {
    ($name:ident, $method:ident) => {
        fn $name<'db, M: Model, T: FilterValue>(
            query: DatabaseQuery<'db, M>,
            field: &'static str,
            value: &str,
        ) -> Result<DatabaseQuery<'db, M>, FilterError> {
            let value = T::parse(value)?;
            Ok(query.filter(ModelField::<M, T>::new(M::table_name(), field).$method(value)))
        }
    };
}

range_filter!(apply_gt, gt);
range_filter!(apply_gte, gte);
range_filter!(apply_lt, lt);
range_filter!(apply_lte, lte);

fn apply_order<'db, M: Model, T>(
    query: DatabaseQuery<'db, M>,
    field: &'static str,
    descending: bool,
) -> DatabaseQuery<'db, M> {
    let field = ModelField::<M, T>::new(M::table_name(), field);
    if descending {
        query.order_by(field.desc())
    } else {
        query.order_by(field.asc())
    }
}
