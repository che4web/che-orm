use crate::{Registry, protocol::Value};
use crate::{
    decode, encode, invalid,
    model::{Descriptor, database},
    protocol::{Entity, Record},
};
use che_orm::{DatabaseValue as D, OrmError, Transaction, Uuid};
use std::{collections::BTreeMap, future::Future, pin::Pin};

pub(crate) const REFERENTIAL_CONFLICT: &str = "referential action would change a preserved row";
pub(crate) type Targets = BTreeMap<(String, Uuid), Option<Record>>;

/// A cascade dependency, materialized before any destructive statement.
pub(crate) struct Dependent {
    pub model: String,
    pub id: Uuid,
    pub record: Record,
    pub field: &'static str,
    pub cascade: bool,
}

pub(crate) async fn dependents(
    tx: &Transaction,
    registry: &Registry,
    parent: &Descriptor,
    id: Uuid,
) -> Result<Vec<Dependent>, OrmError> {
    let mut result = Vec::new();
    for child in registry.models.iter() {
        for column in &child.schema.columns {
            let Some(fk) = &column.references else {
                continue;
            };
            if fk.target_parts().map(|p| p.0) != Some(parent.schema.name) {
                continue;
            }
            let action = fk.on_delete().unwrap_or("restrict").to_ascii_lowercase();
            if action != "cascade" && action != "set null" {
                continue;
            }
            let rows = tx
                .query_sql(
                    format!(
                        "SELECT {} FROM {} WHERE {} = $1 AND {} IS NULL",
                        quote(child.primary),
                        quote(child.schema.name),
                        quote(column.name),
                        quote(child.deleted_at)
                    ),
                    vec![D::Uuid(id)],
                )
                .await?;
            let field = child
                .fields
                .iter()
                .find(|f| f.column == column.name)
                .unwrap()
                .name;
            for row in rows {
                let child_id = row.get::<Uuid>(0)?;
                result.push(Dependent {
                    model: child.name.into(),
                    id: child_id,
                    record: read(tx, child, child_id, false)
                        .await?
                        .ok_or_else(|| invalid("missing dependent row"))?,
                    field,
                    cascade: action == "cascade",
                });
            }
        }
    }
    Ok(result)
}

/// Prevent SQL FK actions from destroying locally preserved rows. If a child
/// must move to another parent, its own write has to succeed before this delete.
pub(crate) fn guard_delete<'a>(
    tx: &'a Transaction,
    registry: &'a Registry,
    d: &'a Descriptor,
    id: Uuid,
    targets: &'a Targets,
) -> che_orm::TransactionFuture<'a, ()> {
    Box::pin(async move {
        for child in dependents(tx, registry, d, id).await? {
            match targets.get(&(child.model.clone(), child.id)) {
                Some(None) => {
                    if child.cascade {
                        guard_delete(tx, registry, registry.get(&child.model)?, child.id, targets)
                            .await?;
                    }
                }
                Some(Some(target))
                    if !child.cascade && target.get(child.field) == Some(&Value::Null) => {}
                _ => return Err(invalid(REFERENTIAL_CONFLICT)),
            }
        }
        Ok(())
    })
}

pub(crate) fn application_conflict(error: &OrmError) -> bool {
    error.constraint_kind().is_some()
        || matches!(error, OrmError::Configuration(message) if message == REFERENTIAL_CONFLICT)
}

/// Try statements in their preferred order, postponing those blocked by FK or
/// UNIQUE dependencies. Savepoints are essential on PostgreSQL: a failed
/// statement must not leave the containing transaction aborted.
pub(crate) async fn ordered<F>(tx: &Transaction, count: usize, action: F) -> Result<(), OrmError>
where
    F: for<'a> Fn(
            &'a Transaction,
            usize,
        ) -> Pin<Box<dyn Future<Output = Result<(), OrmError>> + Send + 'a>>
        + Send
        + Sync,
{
    let mut pending: Vec<_> = (0..count).collect();
    while !pending.is_empty() {
        let mut blocked = Vec::new();
        let mut last_error = None;
        for index in &pending {
            tx.execute_sql("SAVEPOINT sync_step", vec![]).await?;
            match action(tx, *index).await {
                Ok(()) => {
                    tx.execute_sql("RELEASE SAVEPOINT sync_step", vec![])
                        .await?;
                }
                Err(error) => {
                    tx.execute_sql("ROLLBACK TO SAVEPOINT sync_step", vec![])
                        .await?;
                    tx.execute_sql("RELEASE SAVEPOINT sync_step", vec![])
                        .await?;
                    if !application_conflict(&error) {
                        return Err(error);
                    }
                    blocked.push(*index);
                    last_error = Some(error);
                }
            }
        }
        if blocked.len() == pending.len() {
            return Err(last_error.unwrap());
        }
        pending = blocked;
    }
    Ok(())
}

pub(crate) fn text(value: impl ToString) -> D {
    D::Text(value.to_string())
}
pub(crate) fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub(crate) async fn read(
    tx: &Transaction,
    d: &Descriptor,
    id: Uuid,
    mirror: bool,
) -> Result<Option<Record>, OrmError> {
    let table = if mirror {
        d.mirror()
    } else {
        d.schema.name.into()
    };
    let columns = d
        .schema
        .columns
        .iter()
        .map(|c| quote(c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let rows = tx
        .query_sql(
            format!(
                "SELECT {columns} FROM {} WHERE {} = $1 AND {} IS NULL",
                quote(&table),
                quote(d.primary),
                quote(d.deleted_at)
            ),
            vec![D::Uuid(id)],
        )
        .await?;
    rows.first().map(|row| d.read_row(row)).transpose()
}

pub(crate) async fn write(
    tx: &Transaction,
    d: &Descriptor,
    id: Uuid,
    record: Option<&Record>,
    mirror: bool,
) -> Result<(), OrmError> {
    let table = quote(&if mirror {
        d.mirror()
    } else {
        d.schema.name.into()
    });
    let Some(record) = record else {
        tx.execute_sql(
            format!("DELETE FROM {table} WHERE {} = $1", quote(d.primary)),
            vec![D::Uuid(id)],
        )
        .await?;
        return Ok(());
    };
    d.validate(record, id, true, false)?;
    let columns = d.fields.iter().map(|f| quote(f.column)).collect::<Vec<_>>();
    let placeholders = (1..=columns.len())
        .map(|n| format!("${n}"))
        .collect::<Vec<_>>();
    let updates = d
        .fields
        .iter()
        .filter(|f| f.column != d.primary)
        .map(|f| format!("{} = excluded.{}", quote(f.column), quote(f.column)))
        .collect::<Vec<_>>();
    let action = if updates.is_empty() {
        "DO NOTHING".into()
    } else {
        format!("DO UPDATE SET {}", updates.join(", "))
    };
    tx.execute_sql(
        format!(
            "INSERT INTO {table} ({}) VALUES ({}) ON CONFLICT ({}) {action}",
            columns.join(", "),
            placeholders.join(", "),
            quote(d.primary)
        ),
        d.fields.iter().map(|f| database(&record[f.name])).collect(),
    )
    .await?;
    Ok(())
}

pub(crate) async fn shadow(tx: &Transaction, d: &Descriptor, id: Uuid) -> Result<Entity, OrmError> {
    let rows = tx
        .query_sql(
            format!(
                "SELECT {}, {}, {}, _sync_epoch FROM {} WHERE {} = $1",
                quote(d.revision),
                quote(d.updated_at),
                quote(d.deleted_at),
                quote(&d.mirror()),
                quote(d.primary)
            ),
            vec![D::Uuid(id)],
        )
        .await?;
    let epoch = rows
        .first()
        .map(|row| row.get::<Option<String>>(3))
        .transpose()?
        .flatten()
        .map(|s| {
            s.parse::<Uuid>()
                .map_err(|_| invalid("invalid shadow epoch"))
        })
        .transpose()?;
    let (revision, updated_at, deleted_at) = match rows.first() {
        Some(row) => (
            row.get::<i64>(0)?,
            row.get::<Option<che_orm::time::OffsetDateTime>>(1)?,
            row.get::<Option<che_orm::time::OffsetDateTime>>(2)?,
        ),
        None => (0, None, None),
    };
    let deleted = rows.is_empty() || deleted_at.is_some();
    let record = if deleted {
        None
    } else {
        read(tx, d, id, true).await?
    };
    if !deleted && record.is_none() {
        return Err(invalid("missing shadow record"));
    }
    Ok(Entity {
        model: d.name.into(),
        id,
        schema_version: d.version,
        revision,
        epoch,
        updated_at,
        deleted_at,
        record,
    })
}

pub(crate) async fn set_shadow(
    tx: &Transaction,
    d: &Descriptor,
    entity: &Entity,
) -> Result<(), OrmError> {
    if let Some(record) = &entity.record {
        write(tx, d, entity.id, Some(record), true).await?;
    } else if entity.revision > 0 {
        let now = entity
            .deleted_at
            .or(entity.updated_at)
            .unwrap_or_else(che_orm::time::OffsetDateTime::now_utc);
        tx.execute_sql(format!("INSERT INTO {} ({}, {}, {}, {}, {}) VALUES ($1,$2,$3,$4,$5) ON CONFLICT ({}) DO UPDATE SET {} = excluded.{}, {} = excluded.{}, {} = excluded.{}", quote(&d.mirror()), quote(d.primary), quote(d.revision), quote(d.updated_at), quote(d.deleted_at), quote(d.scope), quote(d.primary), quote(d.revision), quote(d.revision), quote(d.updated_at), quote(d.updated_at), quote(d.deleted_at), quote(d.deleted_at)), vec![D::Uuid(entity.id), D::Integer(entity.revision), D::DateTime(entity.updated_at.unwrap_or(now)), D::DateTime(now), text("")]).await?;
    }
    if let Some(epoch) = entity.epoch {
        tx.execute_sql(
            format!(
                "UPDATE {} SET _sync_epoch = $1 WHERE {} = $2",
                quote(&d.mirror()),
                quote(d.primary)
            ),
            vec![text(epoch), D::Uuid(entity.id)],
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn mark(tx: &Transaction, model: &str, id: Uuid) -> Result<(), OrmError> {
    tx.execute_sql("INSERT INTO _sync_dirty (model, entity_id) VALUES ($1,$2) ON CONFLICT (model, entity_id) DO NOTHING", vec![text(model), text(id)]).await?;
    Ok(())
}

fn delete_intent_key(model: &str, id: Uuid) -> String {
    format!("local_delete:{model}:{id}")
}

/// A missing current row can also mean a deferred INSERT. Keep an explicit
/// deletion intent separately from that physical absence, in existing metadata.
pub(crate) async fn deleting(tx: &Transaction, model: &str, id: Uuid) -> Result<bool, OrmError> {
    Ok(meta(tx, &delete_intent_key(model, id))
        .await?
        .unwrap_or(false))
}

pub(crate) async fn set_deleting(
    tx: &Transaction,
    model: &str,
    id: Uuid,
    deleted: bool,
) -> Result<(), OrmError> {
    let key = delete_intent_key(model, id);
    if deleted {
        set_meta(tx, &key, &true).await
    } else {
        tx.execute_sql("DELETE FROM _sync_meta WHERE key = $1", vec![text(key)])
            .await?;
        Ok(())
    }
}

pub(crate) async fn meta<T: serde::de::DeserializeOwned>(
    tx: &Transaction,
    key: &str,
) -> Result<Option<T>, OrmError> {
    let rows = tx
        .query_sql(
            "SELECT value FROM _sync_meta WHERE key = $1",
            vec![text(key)],
        )
        .await?;
    rows.first()
        .map(|r| decode(&r.get::<String>(0)?))
        .transpose()
}

pub(crate) async fn set_meta<T: serde::Serialize>(
    tx: &Transaction,
    key: &str,
    value: &T,
) -> Result<(), OrmError> {
    tx.execute_sql("INSERT INTO _sync_meta (key, value) VALUES ($1,$2) ON CONFLICT (key) DO UPDATE SET value = excluded.value", vec![text(key), text(encode(value)?)]).await?;
    Ok(())
}

pub(crate) async fn savepoint(tx: &Transaction) -> Result<(), OrmError> {
    tx.execute_sql("SAVEPOINT sync_apply", vec![]).await?;
    Ok(())
}
pub(crate) async fn release(tx: &Transaction) -> Result<(), OrmError> {
    tx.execute_sql("RELEASE SAVEPOINT sync_apply", vec![])
        .await?;
    Ok(())
}
pub(crate) async fn rollback(tx: &Transaction) -> Result<(), OrmError> {
    tx.execute_sql("ROLLBACK TO SAVEPOINT sync_apply", vec![])
        .await?;
    release(tx).await
}
