use std::marker::PhantomData;

use crate::{DatabaseValue, ModelField, QueryValue};

#[derive(Debug, Clone, Copy)]
pub(crate) enum QueryOperator {
    Eq,
    Contains,
    Gt,
    Gte,
    Lt,
    Lte,
}

pub(crate) enum QNode {
    Compare {
        field: String,
        operator: QueryOperator,
        value: DatabaseValue,
    },
    #[cfg(feature = "sqlite")]
    AnnotationCompare {
        field: String,
        operator: QueryOperator,
        value: DatabaseValue,
    },
    In {
        field: String,
        values: Vec<DatabaseValue>,
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
    pub(crate) node: QNode,
    pub(crate) _model: PhantomData<M>,
}

impl<M> Q<M> {
    pub fn and(self, other: Self) -> Self {
        Self::new(QNode::And(Box::new(self.node), Box::new(other.node)))
    }
    pub fn or(self, other: Self) -> Self {
        Self::new(QNode::Or(Box::new(self.node), Box::new(other.node)))
    }
    pub fn not(self) -> Self {
        Self::new(QNode::Not(Box::new(self.node)))
    }
    pub(crate) fn new(node: QNode) -> Self {
        Self {
            node,
            _model: PhantomData,
        }
    }
    pub(crate) fn compare(field: &str, operator: QueryOperator, value: DatabaseValue) -> Self {
        Self::new(QNode::Compare {
            field: field.to_string(),
            operator,
            value,
        })
    }
    #[cfg(feature = "sqlite")]
    pub(crate) fn annotation_compare(
        field: &str,
        operator: QueryOperator,
        value: DatabaseValue,
    ) -> Self {
        Self::new(QNode::AnnotationCompare {
            field: field.to_string(),
            operator,
            value,
        })
    }
}

impl<M, T> ModelField<M, T> {
    pub fn eq<V: QueryValue<T>>(self, value: V) -> Q<M> {
        Q::compare(self.db_name(), QueryOperator::Eq, value.into_query_value())
    }
    pub fn gt<V: QueryValue<T>>(self, value: V) -> Q<M> {
        Q::compare(self.db_name(), QueryOperator::Gt, value.into_query_value())
    }
    pub fn gte<V: QueryValue<T>>(self, value: V) -> Q<M> {
        Q::compare(self.db_name(), QueryOperator::Gte, value.into_query_value())
    }
    pub fn lt<V: QueryValue<T>>(self, value: V) -> Q<M> {
        Q::compare(self.db_name(), QueryOperator::Lt, value.into_query_value())
    }
    pub fn lte<V: QueryValue<T>>(self, value: V) -> Q<M> {
        Q::compare(self.db_name(), QueryOperator::Lte, value.into_query_value())
    }
    pub fn in_values<I, V>(self, values: I) -> Q<M>
    where
        I: IntoIterator<Item = V>,
        V: QueryValue<T>,
    {
        Q::new(QNode::In {
            field: self.db_name().to_string(),
            values: values
                .into_iter()
                .map(QueryValue::into_query_value)
                .collect(),
        })
    }
    pub fn is_null(self) -> Q<M> {
        Q::new(QNode::IsNull {
            field: self.db_name().to_string(),
            negated: false,
        })
    }
    pub fn is_not_null(self) -> Q<M> {
        Q::new(QNode::IsNull {
            field: self.db_name().to_string(),
            negated: true,
        })
    }
}

impl<M> ModelField<M, String> {
    pub fn contains<V: QueryValue<String>>(self, value: V) -> Q<M> {
        Q::compare(
            self.db_name(),
            QueryOperator::Contains,
            value.into_query_value(),
        )
    }
}
