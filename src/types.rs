use std::marker::PhantomData;

use crate::{OrderBy, OrderDirection};

#[derive(Debug, Clone, PartialEq)]
pub enum DatabaseValue {
    Integer(i64),
    Text(String),
    Boolean(bool),
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
pub enum Expr {
    Column(ColumnRef),
    Value(DatabaseValue),
    Compare {
        left: Box<Expr>,
        op: CompareOp,
        right: Box<Expr>,
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
    pub fn not(self) -> Expr {
        Expr::Not(Box::new(self))
    }
}

#[derive(Debug, Clone, Copy)]
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
