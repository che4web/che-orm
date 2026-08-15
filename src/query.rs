use std::collections::HashSet;
use std::marker::PhantomData;

use crate::{
    ColumnRef, CreateTableQuery, DatabaseValue, Expr, ModelField, QueryValue, TableRef, TableSchema,
};

#[derive(Debug, Clone, Copy)]
pub enum OrderDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy)]
pub struct OrderBy {
    pub column: ColumnRef,
    pub direction: OrderDirection,
}

#[derive(Debug, Clone)]
pub struct SelectAst {
    pub table: TableRef,
    pub columns: Vec<ColumnRef>,
    pub filter: Option<Expr>,
    pub order_by: Vec<OrderBy>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct InsertValue {
    pub column: ColumnRef,
    pub value: DatabaseValue,
}

#[derive(Debug, Clone)]
pub struct InsertAst {
    pub table: TableRef,
    pub values: Vec<InsertValue>,
    pub returning: Vec<ColumnRef>,
}

#[derive(Debug, Clone)]
pub struct Assignment {
    pub column: ColumnRef,
    pub value: DatabaseValue,
}

#[derive(Debug, Clone)]
pub struct UpdateAst {
    pub table: TableRef,
    pub assignments: Vec<Assignment>,
    pub filter: Option<Expr>,
    pub returning: Vec<ColumnRef>,
}

#[derive(Debug, Clone)]
pub struct DeleteAst {
    pub table: TableRef,
    pub filter: Option<Expr>,
}

#[derive(Debug, Clone)]
pub enum QueryAst {
    Select(SelectAst),
    Insert(InsertAst),
    Update(UpdateAst),
    Delete(DeleteAst),
    CreateTable(crate::CreateTableAst),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryBuildError {
    EmptyInsert,
    EmptyUpdate,
    MissingFilter,
    DuplicateColumn(&'static str),
    ForeignTableColumn {
        column: &'static str,
        table: &'static str,
        expected_table: &'static str,
    },
}

pub trait Model: Sized {
    fn table_name() -> &'static str;
    fn columns() -> &'static [&'static str];
    fn schema() -> TableSchema;
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self>;
    fn insert_values(&self) -> Vec<InsertValue>;
    fn query() -> SelectQuery<Self> {
        SelectQuery::new()
    }
    fn insert() -> InsertQuery<Self> {
        InsertQuery::new()
    }
    fn update() -> UpdateQuery<Self> {
        UpdateQuery::new()
    }
    fn delete() -> DeleteQuery<Self> {
        DeleteQuery::new()
    }
    fn create_table() -> CreateTableQuery<Self> {
        CreateTableQuery::new(Self::schema())
    }
}

pub struct SelectQuery<M> {
    ast: SelectAst,
    _model: PhantomData<M>,
}

impl<M: Model> SelectQuery<M> {
    pub fn new() -> Self {
        let table = M::table_name();
        let columns = M::columns()
            .iter()
            .map(|column| ColumnRef::new(table, column))
            .collect();
        Self {
            ast: SelectAst {
                table: TableRef::new(table),
                columns,
                filter: None,
                order_by: Vec::new(),
                limit: None,
                offset: None,
            },
            _model: PhantomData,
        }
    }
    pub fn filter(mut self, expr: Expr) -> Self {
        self.ast.filter = Some(
            self.ast
                .filter
                .map_or(expr.clone(), |previous| previous.and(expr)),
        );
        self
    }
    pub fn order_by(mut self, order: OrderBy) -> Self {
        self.ast.order_by.push(order);
        self
    }
    pub fn limit(mut self, limit: u64) -> Self {
        self.ast.limit = Some(limit);
        self
    }
    pub fn offset(mut self, offset: u64) -> Self {
        self.ast.offset = Some(offset);
        self
    }
    pub fn into_ast(self) -> Result<QueryAst, QueryBuildError> {
        validate_expr_table(self.ast.filter.as_ref(), self.ast.table.name)?;
        Ok(QueryAst::Select(self.ast))
    }
}

pub struct InsertQuery<M> {
    ast: InsertAst,
    _model: PhantomData<M>,
}

impl<M: Model> InsertQuery<M> {
    pub fn new() -> Self {
        Self {
            ast: InsertAst {
                table: TableRef::new(M::table_name()),
                values: Vec::new(),
                returning: Vec::new(),
            },
            _model: PhantomData,
        }
    }
    pub fn set<T, V>(mut self, field: ModelField<M, T>, value: V) -> Self
    where
        V: QueryValue<T>,
    {
        self.ast.values.push(InsertValue {
            column: field.column(),
            value: value.into_query_value(),
        });
        self
    }
    pub fn returning_all(mut self) -> Self {
        self.ast.returning = M::columns()
            .iter()
            .map(|column| ColumnRef::new(M::table_name(), column))
            .collect();
        self
    }
    pub fn into_ast(self) -> Result<QueryAst, QueryBuildError> {
        if self.ast.values.is_empty() {
            return Err(QueryBuildError::EmptyInsert);
        }
        validate_unique_columns(self.ast.values.iter().map(|value| &value.column))?;
        Ok(QueryAst::Insert(self.ast))
    }
}

pub struct UpdateQuery<M> {
    ast: UpdateAst,
    allow_all: bool,
    _model: PhantomData<M>,
}

impl<M: Model> UpdateQuery<M> {
    pub fn new() -> Self {
        Self {
            ast: UpdateAst {
                table: TableRef::new(M::table_name()),
                assignments: Vec::new(),
                filter: None,
                returning: Vec::new(),
            },
            allow_all: false,
            _model: PhantomData,
        }
    }
    pub fn set<T, V>(mut self, field: ModelField<M, T>, value: V) -> Self
    where
        V: QueryValue<T>,
    {
        self.ast.assignments.push(Assignment {
            column: field.column(),
            value: value.into_query_value(),
        });
        self
    }
    pub fn filter(mut self, expr: Expr) -> Self {
        self.ast.filter = Some(
            self.ast
                .filter
                .map_or(expr.clone(), |previous| previous.and(expr)),
        );
        self
    }
    pub fn returning_all(mut self) -> Self {
        self.ast.returning = M::columns()
            .iter()
            .map(|column| ColumnRef::new(M::table_name(), column))
            .collect();
        self
    }
    pub fn allow_all(mut self) -> Self {
        self.allow_all = true;
        self
    }
    pub fn into_ast(self) -> Result<QueryAst, QueryBuildError> {
        if self.ast.assignments.is_empty() {
            return Err(QueryBuildError::EmptyUpdate);
        }
        if self.ast.filter.is_none() && !self.allow_all {
            return Err(QueryBuildError::MissingFilter);
        }
        validate_unique_columns(
            self.ast
                .assignments
                .iter()
                .map(|assignment| &assignment.column),
        )?;
        validate_expr_table(self.ast.filter.as_ref(), self.ast.table.name)?;
        Ok(QueryAst::Update(self.ast))
    }
}

pub struct DeleteQuery<M> {
    ast: DeleteAst,
    allow_all: bool,
    _model: PhantomData<M>,
}

impl<M: Model> DeleteQuery<M> {
    pub fn new() -> Self {
        Self {
            ast: DeleteAst {
                table: TableRef::new(M::table_name()),
                filter: None,
            },
            allow_all: false,
            _model: PhantomData,
        }
    }
    pub fn filter(mut self, expr: Expr) -> Self {
        self.ast.filter = Some(
            self.ast
                .filter
                .map_or(expr.clone(), |previous| previous.and(expr)),
        );
        self
    }
    pub fn allow_all(mut self) -> Self {
        self.allow_all = true;
        self
    }
    pub fn into_ast(self) -> Result<QueryAst, QueryBuildError> {
        if self.ast.filter.is_none() && !self.allow_all {
            return Err(QueryBuildError::MissingFilter);
        }
        validate_expr_table(self.ast.filter.as_ref(), self.ast.table.name)?;
        Ok(QueryAst::Delete(self.ast))
    }
}

fn validate_unique_columns<'a>(
    columns: impl Iterator<Item = &'a ColumnRef>,
) -> Result<(), QueryBuildError> {
    let mut names = HashSet::new();
    for column in columns {
        if !names.insert(column.name) {
            return Err(QueryBuildError::DuplicateColumn(column.name));
        }
    }
    Ok(())
}

fn validate_expr_table(expr: Option<&Expr>, table: &'static str) -> Result<(), QueryBuildError> {
    let Some(expr) = expr else {
        return Ok(());
    };
    match expr {
        Expr::Column(column) => validate_column_table(column, table),
        Expr::Value(_) => Ok(()),
        Expr::Compare { left, right, .. }
        | Expr::And { left, right }
        | Expr::Or { left, right } => {
            validate_expr_table(Some(left), table)?;
            validate_expr_table(Some(right), table)
        }
        Expr::Not(expr) | Expr::IsNull(expr) | Expr::IsNotNull(expr) => {
            validate_expr_table(Some(expr), table)
        }
    }
}

fn validate_column_table(column: &ColumnRef, table: &'static str) -> Result<(), QueryBuildError> {
    if column.table == table {
        Ok(())
    } else {
        Err(QueryBuildError::ForeignTableColumn {
            column: column.name,
            table: column.table,
            expected_table: table,
        })
    }
}
