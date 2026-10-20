//! Independent, append-only model journals. Legacy scope feeds remain separate.
pub use crate::protocol::model_log::*;
use crate::{
    Registry, SyncModel, decode, encode, invalid,
    model::Descriptor,
    protocol::{Entity, Record, Value},
    storage::{quote, text},
};
use che_orm::{ColumnType, DatabaseValue as D, OrmError, Transaction, Uuid};

/// Internal compatibility marker, not a tenant/workspace supplied by a client.
pub const MODEL_LOG_SCOPE: &str = "__model_logs_v2";

impl Descriptor {
    pub(crate) fn log_table(&self) -> String {
        format!("{}_sync_log", self.schema.name)
    }
    pub(crate) fn log_state_table(&self) -> String {
        format!("{}_sync_log_state", self.schema.name)
    }
    pub(crate) fn service_column(&self, column: &str) -> bool {
        [self.revision, self.updated_at, self.deleted_at, self.scope].contains(&column)
    }
    pub(crate) fn json_value(
        &self,
        field: &str,
        value: &Value,
    ) -> Result<serde_json::Value, OrmError> {
        let column = self.field(field)?.column;
        Ok(match value {
            Value::Integer(value) => serde_json::json!(value.to_string()),
            Value::Real(value) => serde_json::json!(value),
            Value::Text(value) if self.json_columns.contains(&column) => decode(value)?,
            Value::Text(value) => serde_json::json!(value),
            Value::Binary(value) => serde_json::json!(value),
            Value::Boolean(value) => serde_json::json!(value),
            Value::Uuid(value) => serde_json::json!(value),
            Value::DateTime(value) => serde_json::json!(
                value
                    .format(&che_orm::time::format_description::well_known::Rfc3339)
                    .map_err(|error| invalid(error.to_string()))?
            ),
            Value::Null => serde_json::Value::Null,
        })
    }
    pub(crate) fn wire_value(
        &self,
        name: &str,
        value: &serde_json::Value,
        sql_null: bool,
    ) -> Result<Value, OrmError> {
        let field = self.field(name)?;
        let column = self
            .schema
            .columns
            .iter()
            .find(|column| column.name == field.column)
            .unwrap();
        if sql_null || (value.is_null() && !self.json_columns.contains(&column.name)) {
            if !column.nullable {
                return Err(invalid(format!("non-nullable field {name}")));
            }
            return Ok(Value::Null);
        }
        let bad = || invalid(format!("invalid journal value for {name}"));
        Ok(if self.json_columns.contains(&column.name) {
            Value::Text(encode(value)?)
        } else {
            match column.column_type {
                ColumnType::Integer => Value::Integer(
                    value
                        .as_str()
                        .and_then(|value| value.parse().ok())
                        .or_else(|| value.as_i64())
                        .ok_or_else(bad)?,
                ),
                ColumnType::Real => Value::Real(
                    value
                        .as_f64()
                        .filter(|value| value.is_finite())
                        .ok_or_else(bad)?,
                ),
                ColumnType::Text => Value::Text(value.as_str().ok_or_else(bad)?.into()),
                ColumnType::Binary => {
                    Value::Binary(serde_json::from_value(value.clone()).map_err(|_| bad())?)
                }
                ColumnType::Boolean => Value::Boolean(value.as_bool().ok_or_else(bad)?),
                ColumnType::Uuid => {
                    Value::Uuid(value.as_str().ok_or_else(bad)?.parse().map_err(|_| bad())?)
                }
                ColumnType::DateTime => Value::DateTime(
                    che_orm::time::OffsetDateTime::parse(
                        value.as_str().ok_or_else(bad)?,
                        &che_orm::time::format_description::well_known::Rfc3339,
                    )
                    .map_err(|_| bad())?,
                ),
            }
        })
    }
}

impl Registry {
    pub fn model_state(&self, entity: &Entity) -> Result<ModelState, OrmError> {
        let d = self.get(&entity.model)?;
        let mut nulls = Vec::new();
        let values = entity
            .record
            .as_ref()
            .map(|record| {
                record
                    .iter()
                    .filter(|(name, _)| {
                        d.field(name)
                            .is_ok_and(|field| !d.service_column(field.column))
                    })
                    .map(|(name, value)| {
                        if *value == Value::Null && d.json_columns.contains(&d.field(name)?.column)
                        {
                            nulls.push(name.clone());
                        }
                        Ok((name.clone(), d.json_value(name, value)?))
                    })
                    .collect::<Result<serde_json::Map<_, _>, OrmError>>()
            })
            .transpose()?;
        Ok(ModelState {
            model: entity.model.clone(),
            record_id: entity.id,
            revision: entity.revision,
            schema_version: entity.schema_version,
            values,
            sql_null_fields: nulls,
            updated_at: entity.updated_at,
            deleted_at: entity.deleted_at,
        })
    }
    pub fn model_entity(
        &self,
        state: &ModelState,
        history: Option<Uuid>,
    ) -> Result<Entity, OrmError> {
        let d = self.get(&state.model)?;
        if state.schema_version != d.version || state.revision < 0 {
            return Err(invalid("incompatible model state"));
        }
        let record = state
            .values
            .as_ref()
            .map(|values| {
                let mut record = Record::new();
                for field in &d.fields {
                    let value = if field.column == d.revision {
                        Value::Integer(state.revision)
                    } else if field.column == d.scope {
                        Value::Text(MODEL_LOG_SCOPE.into())
                    } else if field.column == d.updated_at {
                        Value::DateTime(
                            state
                                .updated_at
                                .ok_or_else(|| invalid("missing update time"))?,
                        )
                    } else if field.column == d.deleted_at {
                        state.deleted_at.map(Value::DateTime).unwrap_or(Value::Null)
                    } else {
                        d.wire_value(
                            field.name,
                            values
                                .get(field.name)
                                .ok_or_else(|| invalid(format!("missing field {}", field.name)))?,
                            state.sql_null_fields.iter().any(|name| name == field.name),
                        )?
                    };
                    record.insert(field.name.into(), value);
                }
                d.validate(&record, state.record_id, true, false)?;
                Ok::<_, OrmError>(record)
            })
            .transpose()?;
        Ok(Entity {
            model: state.model.clone(),
            id: state.record_id,
            revision: state.revision,
            schema_version: state.schema_version,
            epoch: history,
            updated_at: state.updated_at,
            deleted_at: state.deleted_at,
            record,
        })
    }
    pub fn model_result(
        &self,
        result: crate::protocol::PushResult,
    ) -> Result<ModelWriteResult, OrmError> {
        use crate::protocol::PushResult;
        let states = |entities: Vec<Entity>| {
            entities
                .iter()
                .map(|entity| self.model_state(entity))
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(match result {
            PushResult::Accepted(entities) => ModelWriteResult::Accepted(states(entities)?),
            PushResult::Conflict(entities) => ModelWriteResult::Conflict(states(entities)?),
            PushResult::Rejected { reason } => ModelWriteResult::Rejected {
                code: "validation_failed".into(),
                reason,
            },
        })
    }
    pub fn journal_tables<M: SyncModel>(&self) -> Result<(String, String), OrmError> {
        let d = self.typed::<M>()?;
        Ok((d.log_table(), d.log_state_table()))
    }
}

pub(crate) async fn lock_logs(tx: &Transaction, registry: &Registry) -> Result<(), OrmError> {
    let mut models = registry.models.iter().collect::<Vec<_>>();
    models.sort_by_key(|d| d.schema.name);
    for d in models {
        let table = quote(&d.log_state_table());
        tx.execute_sql(format!("INSERT INTO {table} (key, sequence, history) VALUES (0,0,$1) ON CONFLICT (key) DO NOTHING"), vec![text(Uuid::now_v7())]).await?;
        tx.execute_sql(
            format!("UPDATE {table} SET sequence = sequence WHERE key = 0"),
            vec![],
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn require_baselines(tx: &Transaction, registry: &Registry) -> Result<(), OrmError> {
    for d in registry.models.iter() {
        let state = tx.query_sql(format!("SELECT sequence FROM {} WHERE key = 0", quote(&d.log_state_table())), vec![]).await?;
        if state.first().ok_or_else(|| invalid("uninitialized model log"))?.get::<i64>(0)? == 0
            && !tx.query_sql(format!("SELECT {} FROM {} LIMIT 1", quote(d.primary), quote(d.schema.name)), vec![]).await?.is_empty() {
            return Err(invalid(format!("{} requires explicit model journal backfill", d.name)));
        }
    }
    Ok(())
}

pub(crate) async fn append(
    tx: &Transaction,
    registry: &Registry,
    old: &Entity,
    next: &Entity,
) -> Result<(), OrmError> {
    let d = registry.get(&next.model)?;
    let before = registry.model_state(old)?;
    let after = registry.model_state(next)?;
    let kind = if after.values.is_none() {
        ChangeKind::Delete
    } else if before.values.is_none() {
        ChangeKind::Create
    } else {
        ChangeKind::Update
    };
    let mut diff = Vec::new();
    for field in d
        .fields
        .iter()
        .filter(|field| !d.service_column(field.column))
    {
        let old_value = before
            .values
            .as_ref()
            .and_then(|values| values.get(field.name))
            .cloned();
        let new_value = after
            .values
            .as_ref()
            .and_then(|values| values.get(field.name))
            .cloned();
        let old_sql_null = before.sql_null_fields.iter().any(|name| name == field.name);
        let new_sql_null = after.sql_null_fields.iter().any(|name| name == field.name);
        if old_value != new_value || old_sql_null != new_sql_null {
            diff.push(FieldDiff {
                field: field.name.into(),
                old_value,
                new_value,
                old_sql_null,
                new_sql_null,
            });
        }
    }
    let state = quote(&d.log_state_table());
    let rows = tx
        .query_sql(
            format!("UPDATE {state} SET sequence = sequence + 1 WHERE key = 0 RETURNING sequence"),
            vec![],
        )
        .await?;
    let id: i64 = rows
        .first()
        .ok_or_else(|| invalid("uninitialized model log"))?
        .get(0)?;
    tx.execute_sql(format!("INSERT INTO {} (id, record_id, revision, kind, diff, created_at, schema_version) VALUES ($1,$2,$3,$4,$5,$6,$7)", quote(&d.log_table())), vec![
        D::Integer(id), D::Uuid(next.id), D::Integer(next.revision), text(match kind { ChangeKind::Create => "create", ChangeKind::Update => "update", ChangeKind::Delete => "delete" }),
        text(encode(&diff)?), D::DateTime(next.updated_at.ok_or_else(|| invalid("missing log timestamp"))?), D::Integer(i64::from(next.schema_version)),
    ]).await?;
    Ok(())
}
