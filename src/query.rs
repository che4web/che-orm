use std::collections::HashSet;
use std::marker::PhantomData;

use crate::{
    ColumnRef, CompareOp, CreateTableQuery, DatabaseValue, Expr, ModelField, QueryValue, TableRef,
    TableSchema,
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
    pub joins: Vec<JoinAst>,
    pub filter: Option<Expr>,
    pub order_by: Vec<OrderBy>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct JoinAst {
    pub table: TableRef,
    pub kind: JoinType,
    pub on: Expr,
}

#[derive(Debug, Clone, Copy)]
pub enum JoinType {
    Inner,
    Left,
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
    pub allow_all: bool,
}

#[derive(Debug, Clone)]
pub struct DeleteAst {
    pub table: TableRef,
    pub filter: Option<Expr>,
    pub allow_all: bool,
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
/// Errors detected while finalizing a query builder.
pub enum QueryBuildError {
    EmptyInsert,
    EmptyUpdate,
    PrimaryKeyUpdate,
    MissingFilter,
    DuplicateColumn(&'static str),
    ForeignTableColumn {
        column: &'static str,
        table: &'static str,
        expected_table: &'static str,
    },
    InvalidSchema(String),
}

impl QueryAst {
    /// Validates a query before it is sent to a database.
    pub fn validate(&self) -> Result<(), QueryBuildError> {
        match self {
            Self::Select(query) => {
                validate_expr_table(query.filter.as_ref(), query.table.name)?;
                validate_columns_table(
                    query.table.name,
                    query.order_by.iter().map(|order| &order.column),
                )
            }
            Self::Insert(query) => {
                if query.values.is_empty() {
                    return Err(QueryBuildError::EmptyInsert);
                }
                validate_unique_columns(query.values.iter().map(|value| &value.column))?;
                validate_columns_table(
                    query.table.name,
                    query.values.iter().map(|value| &value.column),
                )
            }
            Self::Update(query) => {
                if query.assignments.is_empty() {
                    return Err(QueryBuildError::EmptyUpdate);
                }
                if query.filter.is_none() && !query.allow_all {
                    return Err(QueryBuildError::MissingFilter);
                }
                validate_unique_columns(
                    query
                        .assignments
                        .iter()
                        .map(|assignment| &assignment.column),
                )?;
                validate_columns_table(
                    query.table.name,
                    query
                        .assignments
                        .iter()
                        .map(|assignment| &assignment.column),
                )?;
                validate_expr_table(query.filter.as_ref(), query.table.name)
            }
            Self::Delete(query) => {
                if query.filter.is_none() && !query.allow_all {
                    return Err(QueryBuildError::MissingFilter);
                }
                validate_expr_table(query.filter.as_ref(), query.table.name)
            }
            Self::CreateTable(query) => query
                .schema
                .validate()
                .map_err(|error| QueryBuildError::InvalidSchema(format!("{error:?}"))),
        }
    }
}

/// Trait implemented by `#[derive(Model)]` for ORM models.
pub trait Model: Sized {
    fn table_name() -> &'static str;
    fn columns() -> &'static [&'static str];
    /// Returns the model's generated integer primary-key field.
    fn primary_key() -> ModelField<Self, i64>;
    fn primary_key_value(&self) -> i64 {
        panic!("Model::primary_key_value must be implemented for eager relations")
    }
    fn schema() -> TableSchema;
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self>;
    fn from_row_at(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<Self> {
        if offset == 0 {
            Self::from_row(row)
        } else {
            Err(rusqlite::Error::InvalidColumnIndex(offset))
        }
    }
    fn insert_values(&self) -> Vec<InsertValue>;
    fn managed_update_values() -> Vec<Assignment> {
        Vec::new()
    }
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

/// Converts a materialized ORM model into an API representation.
pub trait ModelSerializer: serde::Serialize + Sized {
    type Model: Model;
    type Input;

    fn from_input(input: Self::Input) -> Self;
}

/// A typed forward foreign-key relation from one model to another.
#[derive(Debug, Clone, Copy)]
pub struct BelongsTo<From, To, Relation = (), Key = i64> {
    field: ColumnRef,
    getter: fn(&From) -> Key,
    related_name: &'static str,
    _marker: PhantomData<fn() -> (From, To, Relation, Key)>,
}

impl<From, To, Relation, Key> BelongsTo<From, To, Relation, Key> {
    pub const fn new(
        table: &'static str,
        column: &'static str,
        getter: fn(&From) -> Key,
        related_name: &'static str,
    ) -> Self {
        Self {
            field: ColumnRef::new(table, column),
            getter,
            related_name,
            _marker: PhantomData,
        }
    }

    pub const fn field(&self) -> ColumnRef {
        self.field
    }

    pub fn foreign_key(&self, model: &From) -> Key {
        (self.getter)(model)
    }

    pub const fn reverse(&self) -> HasMany<To, From, Relation, Key> {
        HasMany {
            field: self.field,
            getter: self.getter,
            related_name: self.related_name,
            _marker: PhantomData,
        }
    }
}

/// A typed reverse foreign-key relation from one model to many related models.
#[derive(Debug, Clone, Copy)]
pub struct HasMany<From, To, Relation = (), Key = i64> {
    field: ColumnRef,
    getter: fn(&To) -> Key,
    related_name: &'static str,
    _marker: PhantomData<fn() -> (From, To, Relation, Key)>,
}

impl<From, To, Relation, Key> HasMany<From, To, Relation, Key> {
    pub const fn field(&self) -> ColumnRef {
        self.field
    }

    pub fn foreign_key(&self, model: &To) -> Key {
        (self.getter)(model)
    }

    pub const fn related_name(&self) -> &'static str {
        self.related_name
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
                joins: Vec::new(),
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
        let mut tables = vec![self.ast.table.name];
        tables.extend(self.ast.joins.iter().map(|join| join.table.name));
        validate_expr_tables(self.ast.filter.as_ref(), &tables)?;
        for join in &self.ast.joins {
            validate_expr_tables(Some(&join.on), &tables)?;
        }
        Ok(QueryAst::Select(self.ast))
    }

    pub fn into_joined_ast<R: Model, Relation>(
        self,
        relation: BelongsTo<M, R, Relation>,
    ) -> Result<QueryAst, QueryBuildError> {
        self.into_joined_ast_with_kind(relation, JoinType::Inner)
    }

    pub fn into_optional_joined_ast<R: Model, Relation>(
        self,
        relation: BelongsTo<M, R, Relation, Option<i64>>,
    ) -> Result<QueryAst, QueryBuildError> {
        self.into_joined_ast_with_kind(relation, JoinType::Left)
    }

    fn into_joined_ast_with_kind<R: Model, Relation, Key>(
        mut self,
        relation: BelongsTo<M, R, Relation, Key>,
        kind: JoinType,
    ) -> Result<QueryAst, QueryBuildError> {
        self.ast.columns.extend(
            R::columns()
                .iter()
                .map(|column| ColumnRef::new(R::table_name(), column)),
        );
        self.ast.joins.push(JoinAst {
            table: TableRef::new(R::table_name()),
            kind,
            on: Expr::Compare {
                left: Box::new(Expr::Column(relation.field())),
                op: CompareOp::Eq,
                right: Box::new(Expr::Column(R::primary_key().column())),
            },
        });
        self.into_ast()
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
                allow_all: false,
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
    pub fn into_ast(mut self) -> Result<QueryAst, QueryBuildError> {
        for managed in M::managed_update_values() {
            self.ast
                .assignments
                .retain(|assignment| assignment.column.name != managed.column.name);
            self.ast.assignments.push(managed);
        }
        if self.ast.assignments.is_empty() {
            return Err(QueryBuildError::EmptyUpdate);
        }
        if self.ast.filter.is_none() && !self.allow_all {
            return Err(QueryBuildError::MissingFilter);
        }
        let primary_key = M::primary_key().column();
        if self
            .ast
            .assignments
            .iter()
            .any(|assignment| assignment.column == primary_key)
        {
            return Err(QueryBuildError::PrimaryKeyUpdate);
        }
        validate_unique_columns(
            self.ast
                .assignments
                .iter()
                .map(|assignment| &assignment.column),
        )?;
        validate_expr_table(self.ast.filter.as_ref(), self.ast.table.name)?;
        self.ast.allow_all = self.allow_all;
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
                allow_all: false,
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
    pub fn into_ast(mut self) -> Result<QueryAst, QueryBuildError> {
        if self.ast.filter.is_none() && !self.allow_all {
            return Err(QueryBuildError::MissingFilter);
        }
        validate_expr_table(self.ast.filter.as_ref(), self.ast.table.name)?;
        self.ast.allow_all = self.allow_all;
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

fn validate_columns_table<'a>(
    table: &'static str,
    columns: impl Iterator<Item = &'a ColumnRef>,
) -> Result<(), QueryBuildError> {
    for column in columns {
        validate_column_table(column, table)?;
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
        Expr::In { left, .. } => validate_expr_table(Some(left), table),
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

fn validate_expr_tables(
    expr: Option<&Expr>,
    tables: &[&'static str],
) -> Result<(), QueryBuildError> {
    let Some(expr) = expr else {
        return Ok(());
    };
    match expr {
        Expr::Column(column) => {
            if tables.contains(&column.table) {
                Ok(())
            } else {
                Err(QueryBuildError::ForeignTableColumn {
                    column: column.name,
                    table: column.table,
                    expected_table: tables[0],
                })
            }
        }
        Expr::Value(_) => Ok(()),
        Expr::Compare { left, right, .. }
        | Expr::And { left, right }
        | Expr::Or { left, right } => {
            validate_expr_tables(Some(left), tables)?;
            validate_expr_tables(Some(right), tables)
        }
        Expr::In { left, .. } => validate_expr_tables(Some(left), tables),
        Expr::Not(expr) | Expr::IsNull(expr) | Expr::IsNotNull(expr) => {
            validate_expr_tables(Some(expr), tables)
        }
    }
}
