use std::marker::PhantomData;

use time::OffsetDateTime;

use crate::{DbEnum, OrderBy, OrderDirection};

#[derive(Debug, Clone, PartialEq)]
/// Values bound to SQL placeholders.
pub enum DatabaseValue {
    Integer(i64),
    Text(String),
    Boolean(bool),
    DateTime(OffsetDateTime),
    Null,
}

pub trait QueryValue<T> {
    fn into_query_value(self) -> DatabaseValue;
}

impl QueryValue<i64> for i64 {
    fn into_query_value(self) -> DatabaseValue {
        DatabaseValue::Integer(self)
    }
}

impl QueryValue<String> for String {
    fn into_query_value(self) -> DatabaseValue {
        DatabaseValue::Text(self)
    }
}

impl QueryValue<String> for &str {
    fn into_query_value(self) -> DatabaseValue {
        DatabaseValue::Text(self.to_owned())
    }
}

impl QueryValue<bool> for bool {
    fn into_query_value(self) -> DatabaseValue {
        DatabaseValue::Boolean(self)
    }
}

impl QueryValue<OffsetDateTime> for OffsetDateTime {
    fn into_query_value(self) -> DatabaseValue {
        DatabaseValue::DateTime(self)
    }
}

impl<T: DbEnum> QueryValue<T> for T {
    fn into_query_value(self) -> DatabaseValue {
        DatabaseValue::Text(self.as_str().to_owned())
    }
}

impl<T, V> QueryValue<Option<T>> for Option<V>
where
    V: QueryValue<T>,
{
    fn into_query_value(self) -> DatabaseValue {
        self.map_or(DatabaseValue::Null, QueryValue::into_query_value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableRef {
    pub name: &'static str,
}

impl TableRef {
    pub const fn new(name: &'static str) -> Self {
        Self { name }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnRef {
    pub table: &'static str,
    pub name: &'static str,
}

impl ColumnRef {
    pub const fn new(table: &'static str, name: &'static str) -> Self {
        Self { table, name }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
}

#[derive(Debug, Clone)]
/// Boolean and comparison expression tree used by query builders.
pub enum Expr {
    Column(ColumnRef),
    Value(DatabaseValue),
    Compare {
        left: Box<Expr>,
        op: CompareOp,
        right: Box<Expr>,
    },
    Like {
        left: Box<Expr>,
        pattern: Box<Expr>,
    },
    In {
        left: Box<Expr>,
        values: Vec<DatabaseValue>,
    },
    And {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Or {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Not(Box<Expr>),
    IsNull(Box<Expr>),
    IsNotNull(Box<Expr>),
}

impl Expr {
    pub fn and(self, other: Expr) -> Expr {
        Expr::And {
            left: Box::new(self),
            right: Box::new(other),
        }
    }
    pub fn or(self, other: Expr) -> Expr {
        Expr::Or {
            left: Box::new(self),
            right: Box::new(other),
        }
    }
    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Expr {
        Expr::Not(Box::new(self))
    }

    pub fn like(self, pattern: impl Into<String>) -> Expr {
        Expr::Like {
            left: Box::new(self),
            pattern: Box::new(Expr::Value(DatabaseValue::Text(pattern.into()))),
        }
    }
}

#[derive(Debug, Clone, Copy)]
/// A typed reference to one model column.
pub struct ModelField<M, T> {
    table: &'static str,
    column: &'static str,
    _marker: PhantomData<fn() -> (M, T)>,
}

impl<M, T> ModelField<M, T> {
    pub const fn new(table: &'static str, column: &'static str) -> Self {
        Self {
            table,
            column,
            _marker: PhantomData,
        }
    }

    pub const fn column(&self) -> ColumnRef {
        ColumnRef::new(self.table, self.column)
    }

    fn comparison<V>(self, op: CompareOp, value: V) -> Expr
    where
        V: QueryValue<T>,
    {
        Expr::Compare {
            left: Box::new(Expr::Column(self.column())),
            op,
            right: Box::new(Expr::Value(value.into_query_value())),
        }
    }

    pub fn eq<V>(self, value: V) -> Expr
    where
        V: QueryValue<T>,
    {
        self.comparison(CompareOp::Eq, value)
    }
    pub fn in_values<I, V>(self, values: I) -> Expr
    where
        I: IntoIterator<Item = V>,
        V: QueryValue<T>,
    {
        Expr::In {
            left: Box::new(Expr::Column(self.column())),
            values: values
                .into_iter()
                .map(QueryValue::into_query_value)
                .collect(),
        }
    }
    pub fn ne<V>(self, value: V) -> Expr
    where
        V: QueryValue<T>,
    {
        self.comparison(CompareOp::Ne, value)
    }
    pub fn gt<V>(self, value: V) -> Expr
    where
        V: QueryValue<T>,
    {
        self.comparison(CompareOp::Gt, value)
    }
    pub fn gte<V>(self, value: V) -> Expr
    where
        V: QueryValue<T>,
    {
        self.comparison(CompareOp::Gte, value)
    }
    pub fn lt<V>(self, value: V) -> Expr
    where
        V: QueryValue<T>,
    {
        self.comparison(CompareOp::Lt, value)
    }
    pub fn lte<V>(self, value: V) -> Expr
    where
        V: QueryValue<T>,
    {
        self.comparison(CompareOp::Lte, value)
    }
    pub fn is_null(self) -> Expr {
        Expr::IsNull(Box::new(Expr::Column(self.column())))
    }
    pub fn is_not_null(self) -> Expr {
        Expr::IsNotNull(Box::new(Expr::Column(self.column())))
    }

    pub fn contains(self, value: impl Into<String>) -> Expr {
        Expr::Column(self.column()).like(format!("%{}%", value.into()))
    }
    pub fn asc(self) -> OrderBy {
        OrderBy {
            column: self.column(),
            direction: OrderDirection::Asc,
        }
    }
    pub fn desc(self) -> OrderBy {
        OrderBy {
            column: self.column(),
            direction: OrderDirection::Desc,
        }
    }
}
