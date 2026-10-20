use crate::{
    Registry, invalid,
    model::{Descriptor, database},
    storage::*,
};
use che_orm::{
    ColumnRef, CurrentDialect, DatabaseValue as D, Expr, ModelRole, ModelRuntime, ModelWriteResult,
    OrmError, QueryAst, Row, SelectAst, SqlCompiler, TableRef, Transaction, TransactionFuture,
    Uuid,
};
use std::sync::Arc;

pub(crate) fn descriptor<'a>(
    registry: &'a Registry,
    ast: &QueryAst,
) -> Result<&'a Descriptor, OrmError> {
    let table = match ast {
        QueryAst::Insert(ast) => ast.table.name,
        QueryAst::Update(ast) => ast.table.name,
        QueryAst::Delete(ast) => ast.table.name,
        _ => return Err(invalid("expected a model write")),
    };
    registry
        .models
        .iter()
        .find(|d| d.schema.name == table)
        .ok_or_else(|| invalid("unregistered model write"))
}

pub(crate) fn active_filter(d: &Descriptor, table: &'static str, scope: Option<&str>) -> Expr {
    let active = Expr::IsNull(Box::new(Expr::Column(ColumnRef::new(table, d.deleted_at))));
    match scope {
        Some(scope) => active.and(Expr::Compare {
            left: Box::new(Expr::Column(ColumnRef::new(table, d.scope))),
            op: che_orm::CompareOp::Eq,
            right: Box::new(Expr::Value(D::Text(scope.into()))),
        }),
        None => active,
    }
}

pub(crate) fn prepare_read(
    registry: &Registry,
    mut ast: QueryAst,
    scope: Option<&str>,
) -> QueryAst {
    if let QueryAst::Select(select) = &mut ast {
        if let Some(d) = registry
            .models
            .iter()
            .find(|d| d.schema.name == select.table.name)
        {
            let filter = active_filter(d, select.table.name, scope);
            select.filter = Some(match select.filter.take() {
                Some(old) => old.and(filter),
                None => filter,
            });
        }
        for join in &mut select.joins {
            if let Some(d) = registry
                .models
                .iter()
                .find(|d| d.schema.name == join.table.name)
            {
                join.on = join.on.clone().and(active_filter(d, join.alias, scope));
            }
        }
    }
    ast
}

pub(crate) async fn targets(
    tx: &Transaction,
    d: &Descriptor,
    ast: &QueryAst,
    scope: Option<&str>,
) -> Result<Vec<Uuid>, OrmError> {
    let filter = match ast {
        QueryAst::Insert(ast) => {
            return ast
                .values
                .iter()
                .find(|v| v.column.name == d.primary)
                .and_then(|v| match v.value {
                    D::Uuid(id) => Some(vec![id]),
                    _ => None,
                })
                .ok_or_else(|| invalid("sync inserts require a UUID"));
        }
        QueryAst::Update(ast) => ast.filter.clone(),
        QueryAst::Delete(ast) => ast.filter.clone(),
        _ => return Err(invalid("expected a write")),
    };
    let active = active_filter(d, d.schema.name, scope);
    let query = QueryAst::Select(SelectAst {
        table: TableRef::new(d.schema.name),
        columns: vec![ColumnRef::new(d.schema.name, d.primary)],
        joins: vec![],
        filter: Some(match filter {
            Some(filter) => filter.and(active),
            None => active,
        }),
        order_by: vec![],
        limit: None,
        offset: None,
    });
    let compiled = SqlCompiler::<CurrentDialect>::compile(&query);
    tx.query_sql(compiled.sql, compiled.params)
        .await?
        .iter()
        .map(|row| row.get(0).map_err(Into::into))
        .collect()
}

pub(crate) async fn result(
    tx: &Transaction,
    d: &Descriptor,
    ast: &QueryAst,
    ids: &[Uuid],
) -> Result<ModelWriteResult, OrmError> {
    let returning = match ast {
        QueryAst::Insert(ast) => &ast.returning[..],
        QueryAst::Update(ast) => &ast.returning[..],
        _ => &[],
    };
    let mut rows = Vec::new();
    if !returning.is_empty() {
        for id in ids {
            if let Some(record) = read(tx, d, *id, false).await? {
                let values = returning
                    .iter()
                    .map(|column| {
                        let field = d
                            .fields
                            .iter()
                            .find(|field| field.column == column.name)
                            .ok_or_else(|| invalid("unknown returning column"))?;
                        Ok(database(&record[field.name]))
                    })
                    .collect::<Result<Vec<_>, OrmError>>()?;
                rows.push(Row::new(values));
            }
        }
    }
    Ok(ModelWriteResult {
        result: che_orm::ExecuteResult {
            rows_affected: ids.len(),
            last_insert_rowid: None,
        },
        rows,
    })
}

pub(crate) struct ClientRuntime {
    pub registry: Registry,
    pub writes: Arc<tokio::sync::Mutex<()>>,
}

impl ModelRuntime for ClientRuntime {
    fn role(&self, table: &str) -> Option<ModelRole> {
        self.registry
            .models
            .iter()
            .any(|d| d.schema.name == table)
            .then_some(ModelRole::Client)
    }
    fn transaction_lock(&self) -> Option<Arc<tokio::sync::Mutex<()>>> {
        Some(self.writes.clone())
    }
    fn prepare_read(&self, ast: QueryAst) -> QueryAst {
        prepare_read(&self.registry, ast, None)
    }
    fn write<'a>(
        &'a self,
        tx: &'a Transaction,
        ast: QueryAst,
    ) -> TransactionFuture<'a, ModelWriteResult> {
        Box::pin(async move {
            savepoint(tx).await?;
            let write = self.write_inner(tx, &ast).await;
            match write {
                Ok(result) => {
                    release(tx).await?;
                    Ok(result)
                }
                Err(error) => {
                    rollback(tx).await?;
                    Err(error)
                }
            }
        })
    }
}

impl ClientRuntime {
    async fn write_inner(
        &self,
        tx: &Transaction,
        ast: &QueryAst,
    ) -> Result<ModelWriteResult, OrmError> {
        let d = descriptor(&self.registry, ast)?;
        let mut ids = targets(tx, d, ast, None).await?;
        if ids.is_empty()
            && let QueryAst::Delete(delete) = ast
            && let Some(Expr::Compare {
                left,
                op: che_orm::CompareOp::Eq,
                right,
            }) = &delete.filter
            && let (Expr::Column(column), Expr::Value(D::Uuid(id))) = (&**left, &**right)
            && column.name == d.primary
            && shadow(tx, d, *id).await?.record.is_some()
        {
            // Known server rows may have no visible projection when
            // insertion was deferred by a local constraint conflict.
            ids.push(*id);
        }
        match ast {
            QueryAst::Insert(insert) => {
                let mut insert = insert.clone();
                insert.returning.clear();
                let mut supplied = crate::protocol::Record::new();
                for value in &insert.values {
                    let field = d
                        .fields
                        .iter()
                        .find(|field| field.column == value.column.name)
                        .ok_or_else(|| invalid("unknown insert column"))?;
                    if ![d.revision, d.updated_at, d.deleted_at, d.scope].contains(&field.column) {
                        supplied
                            .insert(field.name.into(), crate::model::from_database(&value.value));
                    }
                }
                d.validate(&supplied, ids[0], false, false)?;
                for (column, value) in [
                    (d.revision, D::Integer(0)),
                    (
                        d.updated_at,
                        D::DateTime(che_orm::time::OffsetDateTime::now_utc()),
                    ),
                    (d.deleted_at, D::Null),
                    (d.scope, D::Text(String::new())),
                ] {
                    insert.values.retain(|v| v.column.name != column);
                    insert.values.push(che_orm::InsertValue {
                        column: ColumnRef::new(d.schema.name, column),
                        value,
                    });
                }
                let compiled = SqlCompiler::<CurrentDialect>::compile(&QueryAst::Insert(insert));
                tx.execute_sql(compiled.sql, compiled.params).await?;
                for id in &ids {
                    let record = read(tx, d, *id, false)
                        .await?
                        .ok_or_else(|| invalid("inserted row is missing"))?;
                    d.validate(&record, *id, true, false)?;
                    set_deleting(tx, d.name, *id, false).await?;
                    mark(tx, d.name, *id).await?;
                }
            }
            QueryAst::Update(update) => {
                for assignment in &update.assignments {
                    if assignment.column.name == d.primary {
                        return Err(invalid("primary key is immutable"));
                    }
                    if !d
                        .fields
                        .iter()
                        .any(|field| field.column == assignment.column.name)
                    {
                        return Err(invalid("unknown update column"));
                    }
                }
                for id in &ids {
                    let mut record = read(tx, d, *id, false).await?.unwrap();
                    for assignment in &update.assignments {
                        let field = d
                            .fields
                            .iter()
                            .find(|field| field.column == assignment.column.name)
                            .unwrap();
                        if field.writable {
                            record.insert(
                                field.name.into(),
                                crate::model::from_database(&assignment.value),
                            );
                        }
                    }
                    crate::client::save_local(tx, d, *id, record).await?;
                }
            }
            QueryAst::Delete(_) => {
                for id in &ids {
                    crate::client::delete_local(tx, &self.registry, d.name, *id).await?;
                }
            }
            _ => unreachable!(),
        }
        result(tx, d, ast, &ids).await
    }
}
