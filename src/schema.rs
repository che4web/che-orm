use std::marker::PhantomData;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    Integer,
    Text,
    Boolean,
}

pub trait ColumnTypeOf {
    fn column_type() -> ColumnType;
    fn nullable() -> bool {
        false
    }
}

impl ColumnTypeOf for i64 {
    fn column_type() -> ColumnType {
        ColumnType::Integer
    }
}

impl ColumnTypeOf for String {
    fn column_type() -> ColumnType {
        ColumnType::Text
    }
}

impl ColumnTypeOf for bool {
    fn column_type() -> ColumnType {
        ColumnType::Boolean
    }
}

impl<T: ColumnTypeOf> ColumnTypeOf for Option<T> {
    fn column_type() -> ColumnType {
        T::column_type()
    }
    fn nullable() -> bool {
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKey {
    pub target: &'static str,
    pub on_delete: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSchema {
    pub name: &'static str,
    pub column_type: ColumnType,
    pub nullable: bool,
    pub primary_key: bool,
    pub unique: bool,
    pub default: Option<&'static str>,
    pub check: Option<&'static str>,
    pub references: Option<ForeignKey>,
}

impl ColumnSchema {
    pub fn new(name: &'static str, column_type: ColumnType, nullable: bool) -> Self {
        Self {
            name,
            column_type,
            nullable,
            primary_key: false,
            unique: false,
            default: None,
            check: None,
            references: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema {
    pub name: &'static str,
    pub columns: Vec<ColumnSchema>,
    pub unique_constraints: Vec<Vec<&'static str>>,
    pub indexes: Vec<Vec<&'static str>>,
}

#[derive(Debug, Clone)]
pub struct CreateTableAst {
    pub schema: TableSchema,
}

pub struct CreateTableQuery<M> {
    ast: CreateTableAst,
    _model: PhantomData<M>,
}

impl<M> CreateTableQuery<M> {
    pub fn new(schema: TableSchema) -> Self {
        Self {
            ast: CreateTableAst { schema },
            _model: PhantomData,
        }
    }

    pub fn into_ast(self) -> crate::QueryAst {
        crate::QueryAst::CreateTable(self.ast)
    }
}
