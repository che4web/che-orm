use crate::{
    Registry, SyncModel, decode, encode, invalid,
    model::{Descriptor, database, record},
    protocol::*,
    storage::*,
};
use che_orm::{Database, DatabaseValue as D, IsolationLevel, OrmError, Transaction, Uuid};
use std::sync::Arc;

/// The application binds a server instance to a trusted principal and scope.
/// This policy runs for incoming writes, including implicit cascade effects.
/// Use OrmError::Configuration for permanent business rejections; operational
/// errors propagate without creating a receipt so the operation can be retried.
pub trait AccessPolicy: Send + Sync + 'static {
    fn authorize_scope(&self, principal: &str, scope: &str) -> Result<(), OrmError>;
    fn validate(
        &self,
        principal: &str,
        scope: &str,
        current: &Entity,
        next: &Entity,
    ) -> Result<(), OrmError>;
}

/// Explicit opt-in for trusted local applications and tests.
pub struct AllowAll;
impl AccessPolicy for AllowAll {
    fn authorize_scope(&self, _: &str, _: &str) -> Result<(), OrmError> {
        Ok(())
    }
    fn validate(&self, _: &str, _: &str, _: &Entity, _: &Entity) -> Result<(), OrmError> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct SyncServer {
    db: Database,
    registry: Registry,
    scope: String,
    principal: String,
    policy: Arc<dyn AccessPolicy>,
}

impl SyncServer {
    /// Tables must already be migrated. The principal is trusted transport
    /// context, never a field accepted from a PushRequest.
    pub async fn open(
        db: Database,
        registry: Registry,
        scope: impl Into<String>,
        principal: impl Into<String>,
        policy: Arc<dyn AccessPolicy>,
    ) -> Result<Self, OrmError> {
        let scope = scope.into();
        let principal = principal.into();
        if scope.is_empty() || principal.is_empty() {
            return Err(invalid("empty scope or principal"));
        }
        policy.authorize_scope(&principal, &scope)?;
        db.execute_sql("INSERT INTO _sync_scopes (scope, epoch, sequence) VALUES ($1,$2,0) ON CONFLICT (scope) DO NOTHING", vec![text(&scope), text(Uuid::now_v7())]).await?;
        Ok(Self {
            db,
            registry,
            scope,
            principal,
            policy,
        })
    }

    pub async fn push(&self, request: PushRequest) -> Result<PushResult, OrmError> {
        self.push_internal(request, false).await
    }

    async fn push_internal(
        &self,
        request: PushRequest,
        trusted: bool,
    ) -> Result<PushResult, OrmError> {
        self.policy.authorize_scope(&self.principal, &self.scope)?;
        if request.protocol_version != PROTOCOL_VERSION || request.scope != self.scope {
            return Err(invalid("incompatible push protocol or scope"));
        }
        if request.operation.mutations.is_empty() {
            return Err(invalid("empty operation"));
        }
        let server = self.clone();
        // READ COMMITTED plus a scope row lock yields commit-ordered groups in
        // PostgreSQL. SQLite's write lock provides the same serialization.
        self.db.transaction_with_isolation(IsolationLevel::ReadCommitted, move |tx| Box::pin(async move {
            tx.execute_sql("UPDATE _sync_scopes SET sequence = sequence WHERE scope = $1", vec![text(&server.scope)]).await?;
            let request_json = encode(&request)?;
            let receipt_replica = format!("{}:{}", server.principal.len(), server.principal) + &request.replica_id.to_string();
            let rows = tx.query_sql("SELECT request, result FROM _sync_receipts WHERE scope = $1 AND replica_id = $2 AND operation_id = $3", vec![text(&server.scope), text(&receipt_replica), text(request.operation.id)]).await?;
            if let Some(row) = rows.first() {
                if row.get::<String>(0)? != request_json { return Err(invalid("operation ID reused with different contents")); }
                return decode(&row.get::<String>(1)?);
            }
            let mut seen = std::collections::HashSet::new();
            let mut current = Vec::new(); let mut conflict = false;
            let checked = async {
                for m in &request.operation.mutations {
                    if !seen.insert((&m.model, m.id)) { return Err(invalid("duplicate entity in group")); }
                    let d = server.registry.get(&m.model)?;
                    if m.schema_version != d.version || m.base_revision < 0 { return Err(invalid("incompatible model version")); }
                    if let Some(changes) = &m.changes {
                        d.validate(changes, m.id, false, !trusted)?;
                        if changes.keys().any(|name| d.field(name).is_ok_and(|f| f.column == d.primary)) { return Err(invalid("primary key is immutable")); }
                    }
                    let entity = server.entity(tx, d, m.id).await?;
                    if entity.revision != m.base_revision { conflict = true; }
                    if entity.revision == m.base_revision && entity.revision > 0 && entity.record.is_none() && m.changes.is_some() { return Err(invalid("deleted UUID cannot be resurrected")); }
                    current.push(entity);
                }
                Ok::<_, OrmError>(())
            }.await;
            let result = if let Err(error) = checked {
                if !matches!(error, OrmError::Configuration(_)) { return Err(error); }
                PushResult::Rejected { reason: error.to_string() }
            } else if conflict {
                PushResult::Conflict(current)
            } else {
                // Capture only touched records and dependent cascade effects.
                let mut before = current.clone();
                for m in request.operation.mutations.iter().filter(|m| m.changes.is_none()) {
                    collect_dependents(tx, &server, &m.model, m.id, &mut before).await?;
                }
                savepoint(tx).await?;
                let application = async {
                    let mut plan = Vec::new();
                    for d in server.registry.models.iter() {
                        for m in request.operation.mutations.iter().filter(|m| m.model == d.name && m.changes.is_some()) {
                            server.check_references(tx, d, m.changes.as_ref().unwrap(), &request.operation.mutations).await?;
                            plan.push(m.clone());
                        }
                    }
                    for d in server.registry.models.iter().rev() {
                        for m in request.operation.mutations.iter().filter(|m| m.model == d.name && m.changes.is_none()) {
                            plan.push(m.clone());
                        }
                    }
                    let plan = Arc::new(plan);
                    let writer = server.clone();
                    let mutations = Arc::new(request.operation.mutations.clone());
                    ordered(tx, plan.len(), move |tx, index| {
                        let plan = plan.clone(); let server = writer.clone(); let mutations = mutations.clone();
                        Box::pin(async move {
                            let m = &plan[index];
                            let d = server.registry.get(&m.model)?;
                            if m.changes.is_some() { apply_patch(tx, d, m).await }
                            else {
                                guard_moves(tx, &server.registry, d, m.id, &mutations).await?;
                                write(tx, d, m.id, None, false).await
                            }
                        })
                    }).await?;
                    let mut affected = Vec::new();
                    for old in &before {
                        let d = server.registry.get(&old.model)?;
                        let new_record = read(tx, d, old.id, false).await?;
                        if new_record != old.record {
                            let next = Entity { record: new_record, revision: old.revision.checked_add(1).ok_or_else(|| invalid("revision overflow"))?, ..old.clone() };
                            server.policy.validate(&server.principal, &server.scope, old, &next)?;
                            affected.push(next);
                        }
                    }
                    for (m, old) in request.operation.mutations.iter().zip(&current) {
                        if affected.iter().any(|e| e.model == m.model && e.id == m.id) { continue; }
                        let d = server.registry.get(&m.model)?;
                        let next = Entity { record: read(tx, d, m.id, false).await?, revision: old.revision.checked_add(1).ok_or_else(|| invalid("revision overflow"))?, ..old.clone() };
                        server.policy.validate(&server.principal, &server.scope, old, &next)?;
                        affected.push(next);
                    }
                    for e in &affected {
                        if let Some(record) = &e.record {
                            server.check_references(tx, server.registry.get(&e.model)?, record, &request.operation.mutations).await?;
                        }
                        tx.execute_sql("INSERT INTO _sync_entities (model, entity_id, scope, revision, deleted) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (model, entity_id) DO UPDATE SET revision = excluded.revision, deleted = excluded.deleted", vec![text(&e.model), text(e.id), text(&server.scope), D::Integer(e.revision), D::Integer(i64::from(e.record.is_none()))]).await?;
                    }
                    tx.execute_sql("UPDATE _sync_scopes SET sequence = sequence + 1 WHERE scope = $1", vec![text(&server.scope)]).await?;
                    let cursor = server.cursor(tx).await?;
                    let group = ChangeGroup { sequence: cursor.sequence, entities: affected.clone() };
                    tx.execute_sql("INSERT INTO _sync_changes (scope, sequence, payload) VALUES ($1,$2,$3)", vec![text(&server.scope), D::Integer(cursor.sequence), text(encode(&group)?)]).await?;
                    Ok::<_, OrmError>(affected)
                }.await;
                match application {
                    Ok(affected) => { release(tx).await?; PushResult::Accepted(affected) }
                    Err(error) => {
                        rollback(tx).await?;
                        // Operational failures must be retried, not persisted as
                        // permanent business rejections.
                        if error.constraint_kind().is_none() && !matches!(error, OrmError::Configuration(_)) { return Err(error); }
                        PushResult::Rejected { reason: error.to_string() }
                    }
                }
            };
            tx.execute_sql("INSERT INTO _sync_receipts (scope, replica_id, operation_id, request, result) VALUES ($1,$2,$3,$4,$5)", vec![text(&server.scope), text(receipt_replica), text(request.operation.id), text(request_json), text(encode(&result)?)]).await?;
            Ok(result)
        })).await
    }

    pub async fn pull(&self, request: PullRequest) -> Result<PullResult, OrmError> {
        self.policy.authorize_scope(&self.principal, &self.scope)?;
        if request.protocol_version != PROTOCOL_VERSION
            || request.cursor.scope != self.scope
            || request.cursor.sequence < 0
            || request.limit == 0
            || request.limit > 1000
        {
            return Err(invalid("invalid pull request"));
        }
        let server = self.clone();
        self.db.transaction_with_isolation(IsolationLevel::RepeatableRead, move |tx| Box::pin(async move {
            let current = server.cursor(tx).await?;
            if current.epoch != request.cursor.epoch || request.cursor.sequence > current.sequence { return Ok(PullResult::ResetRequired); }
            let rows = tx.query_sql("SELECT payload FROM _sync_changes WHERE scope = $1 AND sequence > $2 ORDER BY sequence LIMIT $3", vec![text(&server.scope), D::Integer(request.cursor.sequence), D::Integer(i64::from(request.limit))]).await?;
            let groups: Vec<ChangeGroup> = rows.iter().map(|row| decode(&row.get::<String>(0)?)).collect::<Result<_, _>>()?;
            let sequence = groups.last().map_or(request.cursor.sequence, |g| g.sequence);
            // A missing interval means retained history is no longer sufficient.
            if groups.iter().enumerate().any(|(index, group)| group.sequence != request.cursor.sequence + index as i64 + 1) || (groups.is_empty() && sequence < current.sequence) { return Ok(PullResult::ResetRequired); }
            Ok(PullResult::Changes { groups, cursor: Cursor { sequence, ..current.clone() }, has_more: sequence < current.sequence })
        })).await
    }

    pub async fn snapshot(&self) -> Result<Snapshot, OrmError> {
        self.policy.authorize_scope(&self.principal, &self.scope)?;
        let server = self.clone();
        self.db
            .transaction_with_isolation(IsolationLevel::RepeatableRead, move |tx| {
                Box::pin(async move {
                    let cursor = server.cursor(tx).await?;
                    let entities = server.inventory(tx).await?;
                    Ok(Snapshot { entities, cursor })
                })
            })
            .await
    }

    /// Server-side writes use the same revision, policy and journal path.
    pub async fn save<M: SyncModel>(&self, model: &M) -> Result<PushResult, OrmError> {
        let d = self.registry.typed::<M>()?.clone();
        let id = model.primary_key_value();
        let registry = self.clone();
        let old = self
            .db
            .transaction(move |tx| Box::pin(async move { registry.entity(tx, &d, id).await }))
            .await?;
        let data = record(model)?;
        let d = self.registry.typed::<M>()?;
        let changes = d
            .fields
            .iter()
            .filter(|f| {
                let column = d
                    .schema
                    .columns
                    .iter()
                    .find(|c| c.name == f.column)
                    .unwrap();
                !column.primary_key && !column.auto_now && !column.auto_now_add
            })
            .map(|f| (f.name.into(), data[f.name].clone()))
            .collect();
        self.push_internal(
            PushRequest {
                protocol_version: PROTOCOL_VERSION,
                scope: self.scope.clone(),
                replica_id: Uuid::nil(),
                operation: Operation {
                    id: Uuid::now_v7(),
                    mutations: vec![Mutation {
                        model: M::SYNC_NAME.into(),
                        id,
                        schema_version: M::SYNC_VERSION,
                        base_revision: old.revision,
                        changes: Some(changes),
                    }],
                },
            },
            true,
        )
        .await
    }

    pub async fn delete<M: SyncModel>(&self, id: Uuid) -> Result<PushResult, OrmError> {
        let d = self.registry.typed::<M>()?.clone();
        let server = self.clone();
        let old = self
            .db
            .transaction(move |tx| Box::pin(async move { server.entity(tx, &d, id).await }))
            .await?;
        self.push(PushRequest {
            protocol_version: PROTOCOL_VERSION,
            scope: self.scope.clone(),
            replica_id: Uuid::nil(),
            operation: Operation {
                id: Uuid::now_v7(),
                mutations: vec![Mutation {
                    model: M::SYNC_NAME.into(),
                    id,
                    schema_version: M::SYNC_VERSION,
                    base_revision: old.revision,
                    changes: None,
                }],
            },
        })
        .await
    }

    async fn cursor(&self, tx: &Transaction) -> Result<Cursor, OrmError> {
        let rows = tx
            .query_sql(
                "SELECT epoch, sequence FROM _sync_scopes WHERE scope = $1",
                vec![text(&self.scope)],
            )
            .await?;
        let row = rows.first().ok_or_else(|| invalid("missing sync scope"))?;
        Ok(Cursor {
            scope: self.scope.clone(),
            epoch: row
                .get::<String>(0)?
                .parse()
                .map_err(|_| invalid("invalid epoch"))?,
            sequence: row.get(1)?,
        })
    }

    async fn entity(&self, tx: &Transaction, d: &Descriptor, id: Uuid) -> Result<Entity, OrmError> {
        let rows = tx.query_sql("SELECT scope, revision, deleted FROM _sync_entities WHERE model = $1 AND entity_id = $2", vec![text(d.name), text(id)]).await?;
        let revision = if let Some(row) = rows.first() {
            if row.get::<String>(0)? != self.scope {
                return Err(invalid("entity belongs to another scope"));
            }
            row.get(1)?
        } else {
            0
        };
        let record = read(tx, d, id, false).await?;
        if revision == 0 && record.is_some() {
            return Err(invalid(
                "untracked server record; import through sync facade",
            ));
        }
        Ok(Entity {
            model: d.name.into(),
            id,
            schema_version: d.version,
            revision,
            record,
        })
    }

    async fn inventory(&self, tx: &Transaction) -> Result<Vec<Entity>, OrmError> {
        let rows = tx.query_sql("SELECT model, entity_id FROM _sync_entities WHERE scope = $1 ORDER BY model, entity_id", vec![text(&self.scope)]).await?;
        let mut entities = Vec::new();
        for row in rows {
            let model = row.get::<String>(0)?;
            let id = row
                .get::<String>(1)?
                .parse()
                .map_err(|_| invalid("invalid UUID"))?;
            entities.push(self.entity(tx, self.registry.get(&model)?, id).await?);
        }
        Ok(entities)
    }

    async fn check_references(
        &self,
        tx: &Transaction,
        d: &Descriptor,
        changes: &Record,
        mutations: &[Mutation],
    ) -> Result<(), OrmError> {
        for field in &d.fields {
            let Some(Value::Uuid(id)) = changes.get(field.name) else {
                continue;
            };
            let column = d
                .schema
                .columns
                .iter()
                .find(|c| c.name == field.column)
                .unwrap();
            let Some(fk) = &column.references else {
                continue;
            };
            let table = fk.target_parts().unwrap().0;
            let parent = self
                .registry
                .models
                .iter()
                .find(|d| d.schema.name == table)
                .unwrap();
            let rows = tx
                .query_sql(
                    "SELECT scope FROM _sync_entities WHERE model = $1 AND entity_id = $2",
                    vec![text(parent.name), text(id)],
                )
                .await?;
            if let Some(row) = rows.first() {
                if row.get::<String>(0)? != self.scope {
                    return Err(invalid("cross-scope foreign key"));
                }
            } else if !mutations.iter().any(|m| {
                m.model == parent.name && m.id == *id && m.base_revision == 0 && m.changes.is_some()
            }) {
                return Err(invalid("foreign key target is not tracked in this scope"));
            }
        }
        Ok(())
    }
}

fn guard_moves<'a>(
    tx: &'a Transaction,
    registry: &'a Registry,
    parent: &'a Descriptor,
    id: Uuid,
    mutations: &'a [Mutation],
) -> che_orm::TransactionFuture<'a, ()> {
    Box::pin(async move {
        for child in dependents(tx, registry, parent, id).await? {
            if mutations
                .iter()
                .find(|m| m.model == child.model && m.id == child.id)
                .and_then(|m| m.changes.as_ref())
                .and_then(|c| c.get(child.field))
                .is_some_and(|value| *value != Value::Uuid(id))
            {
                return Err(invalid(REFERENTIAL_CONFLICT));
            }
            if child.cascade {
                guard_moves(
                    tx,
                    registry,
                    registry.get(&child.model)?,
                    child.id,
                    mutations,
                )
                .await?;
            }
        }
        Ok(())
    })
}

fn collect_dependents<'a>(
    tx: &'a Transaction,
    server: &'a SyncServer,
    model: &'a str,
    id: Uuid,
    entities: &'a mut Vec<Entity>,
) -> che_orm::TransactionFuture<'a, ()> {
    Box::pin(async move {
        let parent = server.registry.get(model)?;
        for child in server.registry.models.iter() {
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
                            "SELECT {} FROM {} WHERE {} = $1",
                            quote(child.primary),
                            quote(child.schema.name),
                            quote(column.name)
                        ),
                        vec![D::Uuid(id)],
                    )
                    .await?;
                for row in rows {
                    let child_id = row.get::<Uuid>(0)?;
                    if !entities
                        .iter()
                        .any(|e| e.model == child.name && e.id == child_id)
                    {
                        entities.push(server.entity(tx, child, child_id).await?);
                    }
                    if action == "cascade" {
                        collect_dependents(tx, server, child.name, child_id, entities).await?;
                    }
                }
            }
        }
        Ok(())
    })
}

async fn apply_patch(
    tx: &Transaction,
    d: &Descriptor,
    mutation: &Mutation,
) -> Result<(), OrmError> {
    let changes = mutation.changes.as_ref().unwrap();
    let mut values = changes
        .iter()
        .map(|(name, value)| Ok((d.field(name)?.column, database(value))))
        .collect::<Result<Vec<_>, OrmError>>()?;
    let now = che_orm::time::OffsetDateTime::now_utc();
    for column in &d.schema.columns {
        if column.auto_now || (column.auto_now_add && mutation.base_revision == 0) {
            values.push((column.name, D::DateTime(now)));
        }
    }
    if mutation.base_revision == 0 {
        values.push((d.primary, D::Uuid(mutation.id)));
        let columns = values
            .iter()
            .map(|(name, _)| quote(name))
            .collect::<Vec<_>>()
            .join(", ");
        let placeholders = (1..=values.len())
            .map(|n| format!("${n}"))
            .collect::<Vec<_>>()
            .join(", ");
        tx.execute_sql(
            format!(
                "INSERT INTO {} ({columns}) VALUES ({placeholders})",
                quote(d.schema.name)
            ),
            values.into_iter().map(|(_, v)| v).collect(),
        )
        .await?;
    } else if !values.is_empty() {
        let assignments = values
            .iter()
            .enumerate()
            .map(|(i, (name, _))| format!("{} = ${}", quote(name), i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let index = values.len() + 1;
        let mut params = values.into_iter().map(|(_, v)| v).collect::<Vec<_>>();
        params.push(D::Uuid(mutation.id));
        tx.execute_sql(
            format!(
                "UPDATE {} SET {assignments} WHERE {} = ${index}",
                quote(d.schema.name),
                quote(d.primary)
            ),
            params,
        )
        .await?;
    }
    Ok(())
}
