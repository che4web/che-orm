use crate::{
    Registry, SyncModel, decode, encode, invalid, merge, model::record, protocol::*, storage::*,
};
use che_orm::{Database, OrmError, Transaction, TransactionFuture, Uuid};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Pending {
    pub request: PushRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Clone)]
pub struct SyncClient {
    pub(crate) db: Database,
    pub(crate) registry: Registry,
    pub(crate) scope: String,
    pub(crate) replica_id: Uuid,
    pub(crate) writes: Arc<Mutex<()>>,
    pub(crate) cycles: Arc<Mutex<std::collections::HashMap<Option<String>, Arc<Mutex<()>>>>>,
    pub(crate) operations: Arc<Mutex<std::collections::HashMap<Uuid, Arc<Mutex<()>>>>>,
}

/// Explicit resolution relative to the latest persisted remote revision.
pub enum Resolution {
    Local,
    Remote,
    Record(Option<Record>),
}

impl SyncClient {
    /// Open an already migrated client database. One client owns one database
    /// and one scope. Clones share write and sync-cycle coordination.
    pub async fn open(
        db: Database,
        registry: Registry,
        scope: impl Into<String>,
    ) -> Result<Self, OrmError> {
        let scope = scope.into();
        if scope.is_empty() {
            return Err(invalid("empty scope"));
        }
        let expected = scope.clone();
        let replica_id = db
            .transaction(move |tx| {
                Box::pin(async move {
                    if let Some(existing) = meta::<String>(tx, "scope").await? {
                        if existing != expected {
                            return Err(invalid("client database belongs to another scope"));
                        }
                    } else {
                        set_meta(tx, "scope", &expected).await?;
                    }
                    if let Some(id) = meta(tx, "replica_id").await? {
                        Ok(id)
                    } else {
                        let id = Uuid::now_v7();
                        set_meta(tx, "replica_id", &id).await?;
                        Ok(id)
                    }
                })
            })
            .await?;
        Ok(Self {
            db,
            registry,
            scope,
            replica_id,
            writes: Arc::new(Mutex::new(())),
            cycles: Arc::new(Mutex::new(std::collections::HashMap::new())),
            operations: Arc::new(Mutex::new(std::collections::HashMap::new())),
        })
    }

    pub fn replica_id(&self) -> Uuid {
        self.replica_id
    }
    /// Read through the regular ORM. Writes to synchronized models must use
    /// this client's facade so that dirty tracking remains transactional.
    pub fn database(&self) -> &Database {
        &self.db
    }

    pub async fn transaction<T, F>(&self, action: F) -> Result<T, OrmError>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a SyncTransaction<'a>) -> TransactionFuture<'a, T> + Send + 'static,
    {
        let _guard = self.writes.lock().await;
        let registry = self.registry.clone();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    action(&SyncTransaction {
                        tx,
                        registry: &registry,
                    })
                    .await
                })
            })
            .await
    }

    pub async fn save<M: SyncModel>(&self, model: &M) -> Result<(), OrmError> {
        let _guard = self.writes.lock().await;
        let d = self.registry.typed::<M>()?.clone();
        let id = model.primary_key_value();
        let data = record(model)?;
        self.db
            .transaction(move |tx| Box::pin(async move { save_local(tx, &d, id, data).await }))
            .await
    }

    pub async fn delete<M: SyncModel>(&self, id: Uuid) -> Result<(), OrmError> {
        self.transaction(move |tx| Box::pin(async move { tx.delete::<M>(id).await }))
            .await
    }

    pub async fn conflicts(&self) -> Result<Vec<Conflict>, OrmError> {
        let rows = self
            .db
            .query_sql(
                "SELECT payload FROM _sync_conflicts ORDER BY model, entity_id",
                vec![],
            )
            .await?;
        rows.iter().map(|r| decode(&r.get::<String>(0)?)).collect()
    }

    pub async fn resolve(
        &self,
        model: &str,
        id: Uuid,
        resolution: Resolution,
    ) -> Result<(), OrmError> {
        let _guard = self.writes.lock().await;
        let d = self.registry.get(model)?.clone();
        let registry = self.registry.clone();
        self.db.transaction(move |tx| Box::pin(async move {
            let rows = tx.query_sql("SELECT payload FROM _sync_conflicts WHERE model = $1 AND entity_id = $2", vec![text(d.name), text(id)]).await?;
            let Some(row) = rows.first() else { return Err(invalid("no conflict to resolve")); };
            let conflict: Conflict = decode(&row.get::<String>(0)?)?;
            let remote = shadow(tx, &d, id).await?;
            let target = match resolution {
                Resolution::Local => read(tx, &d, id, false).await?,
                Resolution::Remote => remote.record.clone(),
                Resolution::Record(record) => record,
            };
            if remote.revision > 0 && remote.record.is_none() && target.is_some() {
                return Err(invalid("deleted UUID cannot be resurrected; create a new UUID"));
            }
            let mut target = target;
            if let (Some(target), Some(remote)) = (&mut target, &remote.record) {
                for field in d.fields.iter().filter(|f| !f.writable) { target.insert(field.name.into(), remote[field.name].clone()); }
            }
            if target.is_none() {
                let targets = Targets::from([((d.name.into(), id), None)]);
                guard_delete(tx, &registry, &d, id, &targets).await?;
            }
            write(tx, &d, conflict.id, target.as_ref(), false).await?;
            set_deleting(tx, d.name, id, target.is_none()).await?;
            tx.execute_sql("DELETE FROM _sync_conflicts WHERE model = $1 AND entity_id = $2", vec![text(d.name), text(id)]).await?;
            mark(tx, d.name, id).await
        })).await
    }

    pub(crate) async fn cursor(&self, model: Option<&str>) -> Result<Option<Cursor>, OrmError> {
        let key = cursor_key(model);
        self.db
            .transaction(move |tx| Box::pin(async move { meta(tx, &key).await }))
            .await
    }

    pub(crate) async fn pending(&self, model: Option<&str>) -> Result<Option<Pending>, OrmError> {
        let model = model.map(str::to_owned);
        let registry = self.registry.clone();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    let pending = pending_operations(tx).await?;
                    let owned = pending
                        .iter()
                        .filter(|p| p.model == model)
                        .collect::<Vec<_>>();
                    if owned.len() > 1 {
                        return Err(invalid("multiple outbox groups for one feed"));
                    }
                    if let Some(owned) = owned.first() {
                        return Ok(Some((*owned).clone()));
                    }
                    // A parent may already have an immutable operation (including a lost
                    // reply). Retry that exact operation before creating a child group.
                    if let Some(model) = &model {
                        let selected = selected_entities(tx, &registry, Some(model)).await?;
                        let allowed = registry.related_models(model)?;
                        for operation in pending {
                            let dependencies =
                                operation_dependencies(tx, &registry, &operation.request.operation)
                                    .await?;
                            if operation
                                .request
                                .operation
                                .mutations
                                .iter()
                                .all(|m| allowed.contains(&m.model))
                                && !selected.is_disjoint(&dependencies)
                            {
                                return Ok(Some(operation));
                            }
                        }
                    }
                    Ok(None)
                })
            })
            .await
    }

    pub(crate) async fn prepare(
        &self,
        model: Option<&str>,
        max_mutations: usize,
    ) -> Result<Option<Pending>, OrmError> {
        if max_mutations == 0 {
            return Err(invalid("transport mutation limit must be positive"));
        }
        let _guard = self.writes.lock().await;
        let registry = self.registry.clone();
        let scope = self.scope.clone();
        let replica_id = self.replica_id;
        let model = model.map(str::to_owned);
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    let pending = pending_operations(tx).await?;
                    if pending.iter().any(|p| p.model == model) {
                        return Err(invalid("outbox already prepared"));
                    }
                    if model.is_none() && !pending.is_empty() {
                        return Ok(None);
                    }
                    let selected = selected_entities(tx, &registry, model.as_deref()).await?;
                    let mut required_creates = EntityKeys::new();
                    for (name, id) in &selected {
                        let d = registry.get(name)?;
                        if let Some(record) = read(tx, d, *id, false).await? {
                            for field in &d.fields {
                                let column = d
                                    .schema
                                    .columns
                                    .iter()
                                    .find(|c| c.name == field.column)
                                    .unwrap();
                                if let (Some(fk), Some(Value::Uuid(parent_id))) =
                                    (&column.references, record.get(field.name))
                                {
                                    let parent = registry
                                        .models
                                        .iter()
                                        .find(|d| d.schema.name == fk.target_parts().unwrap().0)
                                        .unwrap();
                                    if shadow(tx, parent, *parent_id).await?.record.is_none() {
                                        required_creates
                                            .insert((parent.name.to_owned(), *parent_id));
                                    }
                                }
                            }
                        }
                    }
                    let send_model = model.as_ref().map(|root| {
                        registry
                            .models
                            .iter()
                            .find(|d| required_creates.iter().any(|(name, _)| name == d.name))
                            .or_else(|| {
                                registry.models.iter().find(|d| {
                                    d.name != root
                                        && selected.iter().any(|(name, _)| name == d.name)
                                })
                            })
                            .map_or_else(|| root.clone(), |d| d.name.to_owned())
                    });
                    let outgoing = entity_dependencies(tx, &registry, &selected).await?;
                    for operation in &pending {
                        let keys: EntityKeys = operation
                            .request
                            .operation
                            .mutations
                            .iter()
                            .map(|m| (m.model.clone(), m.id))
                            .collect();
                        let frozen =
                            operation_dependencies(tx, &registry, &operation.request.operation)
                                .await?;
                        if !selected.is_disjoint(&frozen) || !keys.is_disjoint(&outgoing) {
                            return Ok(None);
                        }
                    }
                    let rows = tx
                        .query_sql("SELECT model, entity_id FROM _sync_dirty", vec![])
                        .await?;
                    for row in tx
                        .query_sql("SELECT model, entity_id FROM _sync_conflicts", vec![])
                        .await?
                    {
                        let id: Uuid = row
                            .get::<String>(1)?
                            .parse()
                            .map_err(|_| invalid("invalid conflict UUID"))?;
                        if model.is_none() || selected.contains(&(row.get::<String>(0)?, id)) {
                            return Ok(None);
                        }
                    }
                    let mut mutations = Vec::new();
                    for d in registry.models.iter() {
                        for row in &rows {
                            if row.get::<String>(0)? != d.name {
                                continue;
                            }
                            let id: Uuid = row
                                .get::<String>(1)?
                                .parse()
                                .map_err(|_| invalid("invalid persisted UUID"))?;
                            let base = shadow(tx, d, id).await?;
                            let local = read(tx, d, id, false).await?;
                            let changes = merge::patch(d, base.record.as_ref(), local.as_ref());
                            let unchanged = match &changes {
                                Some(c) => c.is_empty() && base.record.is_some(),
                                None => base.record.is_none(),
                            };
                            if unchanged {
                                set_deleting(tx, d.name, id, false).await?;
                                tx.execute_sql(
                                    "DELETE FROM _sync_dirty WHERE model = $1 AND entity_id = $2",
                                    vec![text(d.name), text(id)],
                                )
                                .await?;
                                continue;
                            }
                            if !selected.contains(&(d.name.into(), id))
                                || send_model.as_ref().is_some_and(|model| model != d.name)
                                || (model.is_some()
                                    && !required_creates.is_empty()
                                    && !required_creates.contains(&(d.name.into(), id)))
                            {
                                continue;
                            }
                            mutations.push(Mutation {
                                model: d.name.into(),
                                id,
                                schema_version: d.version,
                                base_revision: base.revision,
                                changes,
                            });
                        }
                    }
                    if mutations.is_empty() {
                        return Ok(None);
                    }
                    mutations.sort_by_key(|mutation| mutation.changes.is_some());
                    mutations.truncate(max_mutations);
                    let pending = Pending {
                        model: send_model,
                        request: PushRequest {
                            protocol_version: PROTOCOL_VERSION,
                            scope,
                            replica_id,
                            operation: Operation {
                                id: Uuid::now_v7(),
                                mutations,
                            },
                        },
                    };
                    tx.execute_sql(
                        "INSERT INTO _sync_outbox (operation_id, payload) VALUES ($1,$2)",
                        vec![text(pending.request.operation.id), text(encode(&pending)?)],
                    )
                    .await?;
                    Ok(Some(pending))
                })
            })
            .await
    }

    pub(crate) async fn finish(
        &self,
        pending: Pending,
        result: PushResult,
    ) -> Result<(), OrmError> {
        let _guard = self.writes.lock().await;
        let registry = self.registry.clone();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    let id = pending.request.operation.id;
                    let rows = tx
                        .query_sql(
                            "SELECT payload FROM _sync_outbox WHERE operation_id = $1",
                            vec![text(id)],
                        )
                        .await?;
                    if rows.is_empty() {
                        return Err(invalid("missing pending operation"));
                    }
                    let stored: Pending = decode(&rows[0].get::<String>(0)?)?;
                    if stored.request != pending.request || stored.model != pending.model {
                        return Err(invalid("pending operation contents mismatch"));
                    }
                    let (entities, accepted, rejection) = match result {
                        PushResult::Accepted(e) => (e, true, None),
                        PushResult::Conflict(e) => (e, false, None),
                        PushResult::Rejected { reason } => (vec![], false, Some(reason)),
                    };
                    if rejection.is_none() {
                        let expected = &pending.request.operation.mutations;
                        if expected
                            .iter()
                            .any(|m| !entities.iter().any(|e| e.model == m.model && e.id == m.id))
                        {
                            return Err(invalid("incomplete push response"));
                        }
                    }
                    let mut updates = Vec::new();
                    for mutation in &pending.request.operation.mutations {
                        let d = registry.get(&mutation.model)?;
                        if mutation.schema_version != d.version {
                            return Err(invalid("pending operation model version changed"));
                        }
                        let local = read(tx, d, mutation.id, false).await?;
                        let deleted = deleting(tx, d.name, mutation.id).await? && local.is_none();
                        let current_shadow = shadow(tx, d, mutation.id).await?;
                        if current_shadow.revision != mutation.base_revision {
                            return Err(invalid("pending operation shadow base changed"));
                        }
                        let remote = entities
                            .iter()
                            .find(|e| e.model == mutation.model && e.id == mutation.id)
                            .cloned()
                            .unwrap_or(current_shadow.clone());
                        validate_entity(d, &remote)?;
                        if remote.revision < current_shadow.revision {
                            return Err(invalid("stale push response"));
                        }
                        if accepted && remote.revision <= current_shadow.revision {
                            return Err(invalid("accepted operation did not advance revision"));
                        }
                        // The sole shadow is frozen until this request finishes.
                        // Reconstruct the sent state instead of persisting a
                        // second snapshot in the outbox.
                        let sent = mutation.changes.as_ref().map(|changes| {
                            let mut sent = current_shadow.record.clone().unwrap_or_default();
                            sent.extend(changes.clone());
                            sent
                        });
                        // A blocked acknowledgement must retry against what
                        // was sent, not against the previous server revision.
                        // Complete its writable snapshot with materialized
                        // read-only fields; those never participate in merging.
                        let sent_base = sent.as_ref().map(|sent| {
                            let mut complete = local
                                .as_ref()
                                .or(remote.record.as_ref())
                                .or(current_shadow.record.as_ref())
                                .cloned()
                                .unwrap_or_else(|| sent.clone());
                            for field in d.fields.iter().filter(|f| f.writable) {
                                complete.insert(field.name.into(), sent[field.name].clone());
                            }
                            complete
                        });
                        let base = if accepted {
                            sent.as_ref()
                        } else {
                            current_shadow.record.as_ref()
                        };
                        let (merged, fields) = if accepted {
                            merge::acknowledge(d, base, local.as_ref(), remote.record.as_ref())
                        } else if deleted {
                            merge::delete(base, remote.record.as_ref())
                        } else {
                            merge::merge(d, base, local.as_ref(), remote.record.as_ref())
                        };
                        let cancelled = deleted && remote.record.is_none();
                        let conflict = if !fields.is_empty() || (rejection.is_some() && !cancelled)
                        {
                            Some(Conflict {
                                model: d.name.into(),
                                id: mutation.id,
                                base: current_shadow.record.clone(),
                                local: local.clone(),
                                remote: remote.clone(),
                                fields,
                                reason: rejection.clone(),
                                retryable: false,
                                acknowledgement: None,
                            })
                        } else {
                            None
                        };
                        updates.push(Update {
                            acknowledgement: accepted.then(|| remote.clone()),
                            remote,
                            merged,
                            conflict,
                            previous: local,
                            base: if accepted {
                                sent_base
                            } else {
                                current_shadow.record
                            },
                            received: rejection.is_none(),
                        });
                    }
                    // Accepted groups can include server-side CASCADE / SET NULL
                    // effects beyond the explicitly sent mutations.
                    for remote in entities {
                        if updates
                            .iter()
                            .any(|u| u.remote.model == remote.model && u.remote.id == remote.id)
                        {
                            continue;
                        }
                        let d = registry.get(&remote.model)?;
                        validate_entity(d, &remote)?;
                        let base = shadow(tx, d, remote.id).await?;
                        if remote.revision <= base.revision {
                            continue;
                        }
                        let local = read(tx, d, remote.id, false).await?;
                        let (merged, fields) = merge::merge(
                            d,
                            base.record.as_ref(),
                            local.as_ref(),
                            remote.record.as_ref(),
                        );
                        let conflict = if fields.is_empty() {
                            None
                        } else {
                            Some(Conflict {
                                model: remote.model.clone(),
                                id: remote.id,
                                base: base.record.clone(),
                                local: local.clone(),
                                remote: remote.clone(),
                                fields,
                                reason: None,
                                retryable: false,
                                acknowledgement: None,
                            })
                        };
                        updates.push(Update {
                            acknowledgement: None,
                            remote,
                            merged,
                            conflict,
                            previous: local,
                            base: base.record,
                            received: true,
                        });
                    }
                    apply_updates(tx, &registry, updates).await?;
                    tx.execute_sql(
                        "DELETE FROM _sync_outbox WHERE operation_id = $1",
                        vec![text(id)],
                    )
                    .await?;
                    Ok(())
                })
            })
            .await
    }

    pub(crate) async fn apply(
        &self,
        groups: Vec<ChangeGroup>,
        cursor: Cursor,
        reset: bool,
    ) -> Result<bool, OrmError> {
        let _guard = self.writes.lock().await;
        let registry = self.registry.clone();
        let scope = self.scope.clone();
        self.db.transaction(move |tx| Box::pin(async move {
            if cursor.scope != scope || cursor.sequence < 0 { return Err(invalid("invalid cursor scope or sequence")); }
            let mut incoming = EntityKeys::new();
            for entity in groups.iter().flat_map(|g| &g.entities) {
                let old = shadow(tx, registry.get(&entity.model)?, entity.id).await?;
                if *entity != old { incoming.insert((entity.model.clone(), entity.id)); }
            }
            let incoming_dependencies = entity_dependencies(tx, &registry, &incoming).await?;
            for pending in pending_operations(tx).await? {
                let keys: EntityKeys = pending.request.operation.mutations.iter().map(|m| (m.model.clone(), m.id)).collect();
                let frozen = operation_dependencies(tx, &registry, &pending.request.operation).await?;
                if !incoming.is_disjoint(&frozen) || !keys.is_disjoint(&incoming_dependencies) { return Ok(false); }
            }
            let key = cursor_key(cursor.model.as_deref());
            let allowed = cursor.model.as_deref().map(|model| registry.related_models(model)).transpose()?;
            let required = cursor.model.as_deref().map(|model| registry.feed_models(model)).transpose()?;
            if groups.iter().flat_map(|g| &g.entities).any(|e| allowed.as_ref().is_some_and(|a| !a.contains(&e.model))) { return Err(invalid("incoming entity outside model feed")); }
            if let Some(old) = meta::<Cursor>(tx, &key).await? {
                if !reset && (old.epoch != cursor.epoch || cursor.sequence < old.sequence) { return Err(invalid("stale cursor")); }
            }
            if reset {
                for d in registry.models.iter() {
                    let known = tx.query_sql(format!("SELECT {} FROM {} WHERE {} > 0", quote(d.primary), quote(&d.mirror()), quote(d.revision)), vec![]).await?;
                    for row in known {
                    let model = d.name.to_owned(); let id = row.get::<Uuid>(0)?;
                    if required.as_ref().is_some_and(|required| !required.contains(&model)) { continue; }
                    if !groups.iter().flat_map(|g| &g.entities).any(|e| e.model == model && e.id == id) {
                        return Err(invalid("snapshot omitted a known entity; scope eviction requires an explicit tombstone"));
                    }
                    }
                }
            }
            // Preserve committed group order rather than flattening a page.
            // Each old shadow is read before replacing it, in this transaction.
            for mut group in groups {
                // Model feeds overlap on parents. A snapshot fetched before a
                // concurrent acknowledgement must not roll its shadow back.
                for entity in &mut group.entities { entity.epoch = Some(cursor.epoch); }
                let updates = reconcile(tx, &registry, group.entities, reset).await?;
                apply_updates(tx, &registry, updates).await?;
            }
            retry_blocked(tx, &registry).await?;
            set_meta(tx, &key, &cursor).await?;
            Ok(true)
        })).await
    }

    pub(crate) async fn feed_status(
        &self,
        model: Option<&str>,
    ) -> Result<(usize, bool, bool), OrmError> {
        let registry = self.registry.clone();
        let model = model.map(str::to_owned);
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    let selected = selected_entities(tx, &registry, model.as_deref()).await?;
                    let mut conflicts = 0;
                    for row in tx
                        .query_sql("SELECT model, entity_id FROM _sync_conflicts", vec![])
                        .await?
                    {
                        let name = row.get::<String>(0)?;
                        let id: Uuid = row
                            .get::<String>(1)?
                            .parse()
                            .map_err(|_| invalid("invalid conflict UUID"))?;
                        if model.is_none()
                            || model.as_deref() == Some(name.as_str())
                            || selected.contains(&(name, id))
                        {
                            conflicts += 1;
                        }
                    }
                    let has_work = selected
                        .iter()
                        .any(|(name, _)| model.as_ref().is_none_or(|model| name == model));
                    let outgoing = entity_dependencies(tx, &registry, &selected).await?;
                    let mut blocked = false;
                    for pending in pending_operations(tx).await? {
                        let keys: EntityKeys = pending
                            .request
                            .operation
                            .mutations
                            .iter()
                            .map(|m| (m.model.clone(), m.id))
                            .collect();
                        let frozen =
                            operation_dependencies(tx, &registry, &pending.request.operation)
                                .await?;
                        blocked |= !selected.is_disjoint(&frozen) || !keys.is_disjoint(&outgoing);
                    }
                    Ok((conflicts, has_work, blocked))
                })
            })
            .await
    }
}

type EntityKeys = std::collections::BTreeSet<(String, Uuid)>;

async fn operation_dependencies(
    tx: &Transaction,
    registry: &Registry,
    operation: &Operation,
) -> Result<EntityKeys, OrmError> {
    let keys = operation
        .mutations
        .iter()
        .map(|m| (m.model.clone(), m.id))
        .collect();
    let mut result = entity_dependencies(tx, registry, &keys).await?;
    for mutation in &operation.mutations {
        let d = registry.get(&mutation.model)?;
        for field in &d.fields {
            let column = d
                .schema
                .columns
                .iter()
                .find(|c| c.name == field.column)
                .unwrap();
            if let (Some(fk), Some(Value::Uuid(id))) = (
                &column.references,
                mutation
                    .changes
                    .as_ref()
                    .and_then(|patch| patch.get(field.name)),
            ) {
                let parent = registry
                    .models
                    .iter()
                    .find(|d| d.schema.name == fk.target_parts().unwrap().0)
                    .unwrap();
                result.insert((parent.name.into(), *id));
            }
        }
    }
    Ok(result)
}

async fn entity_dependencies(
    tx: &Transaction,
    registry: &Registry,
    keys: &EntityKeys,
) -> Result<EntityKeys, OrmError> {
    let mut result = keys.clone();
    for (name, id) in keys {
        let d = registry.get(name)?;
        let local = read(tx, d, *id, false).await?;
        let base = shadow(tx, d, *id).await?;
        for record in [local.as_ref(), base.record.as_ref()].into_iter().flatten() {
            for field in &d.fields {
                let column = d
                    .schema
                    .columns
                    .iter()
                    .find(|c| c.name == field.column)
                    .unwrap();
                if let (Some(fk), Some(Value::Uuid(parent))) =
                    (&column.references, record.get(field.name))
                {
                    let table = fk.target_parts().unwrap().0;
                    let parent_model = registry
                        .models
                        .iter()
                        .find(|d| d.schema.name == table)
                        .unwrap();
                    result.insert((parent_model.name.into(), *parent));
                }
            }
        }
    }
    Ok(result)
}

fn cursor_key(model: Option<&str>) -> String {
    model.map_or_else(|| "cursor".into(), |model| format!("cursor:model:{model}"))
}

async fn pending_operations(tx: &Transaction) -> Result<Vec<Pending>, OrmError> {
    tx.query_sql(
        "SELECT payload FROM _sync_outbox ORDER BY operation_id",
        vec![],
    )
    .await?
    .iter()
    .map(|row| decode(&row.get::<String>(0)?))
    .collect()
}

/// Select this model's dirty rows and only necessary parent creates / child
/// moves and deletions. Existing parent edits remain in their own model queue.
async fn selected_entities(
    tx: &Transaction,
    registry: &Registry,
    model: Option<&str>,
) -> Result<std::collections::BTreeSet<(String, Uuid)>, OrmError> {
    let mut dirty = std::collections::BTreeSet::new();
    for row in tx
        .query_sql("SELECT model, entity_id FROM _sync_dirty", vec![])
        .await?
    {
        let name = row.get::<String>(0)?;
        let id = row
            .get::<String>(1)?
            .parse()
            .map_err(|_| invalid("invalid dirty UUID"))?;
        let descriptor = registry.get(&name)?;
        let base = shadow(tx, descriptor, id).await?;
        let local = read(tx, descriptor, id, false).await?;
        let patch = merge::patch(descriptor, base.record.as_ref(), local.as_ref());
        let unchanged = match patch {
            Some(patch) => patch.is_empty() && base.record.is_some(),
            None => base.record.is_none(),
        };
        if !unchanged {
            dirty.insert((name, id));
        }
    }
    let Some(model) = model else {
        return Ok(dirty);
    };
    registry.get(model)?;
    let mut selected = dirty
        .iter()
        .filter(|(name, _)| name == model)
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    loop {
        let before = selected.len();
        for (name, id) in selected.clone() {
            let d = registry.get(&name)?;
            if let Some(local) = read(tx, d, id, false).await? {
                for field in &d.fields {
                    let column = d
                        .schema
                        .columns
                        .iter()
                        .find(|c| c.name == field.column)
                        .unwrap();
                    let Some(fk) = &column.references else {
                        continue;
                    };
                    let Some(Value::Uuid(parent_id)) = local.get(field.name) else {
                        continue;
                    };
                    let table = fk.target_parts().unwrap().0;
                    let parent = registry
                        .models
                        .iter()
                        .find(|d| d.schema.name == table)
                        .unwrap();
                    if shadow(tx, parent, *parent_id).await?.record.is_none() {
                        let key = (parent.name.to_owned(), *parent_id);
                        if !dirty.contains(&key) {
                            return Err(invalid("missing tracked local FK dependency"));
                        }
                        selected.insert(key);
                    }
                }
            } else {
                // A parent delete must preserve local child moves made before
                // deletion, and send dirty dependent tombstones atomically.
                for (child_name, child_id) in &dirty {
                    let child = registry.get(child_name)?;
                    let base = shadow(tx, child, *child_id).await?;
                    let local = read(tx, child, *child_id, false).await?;
                    for field in &child.fields {
                        let column = child
                            .schema
                            .columns
                            .iter()
                            .find(|c| c.name == field.column)
                            .unwrap();
                        if column
                            .references
                            .as_ref()
                            .is_some_and(|fk| fk.target_parts().unwrap().0 == d.schema.name)
                            && [base.record.as_ref(), local.as_ref()]
                                .into_iter()
                                .flatten()
                                .any(|r| r.get(field.name) == Some(&Value::Uuid(id)))
                        {
                            selected.insert((child_name.clone(), *child_id));
                        }
                    }
                }
            }
        }
        if selected.len() == before {
            return Ok(selected);
        }
    }
}

pub struct SyncTransaction<'a> {
    tx: &'a Transaction,
    registry: &'a Registry,
}

impl SyncTransaction<'_> {
    pub async fn get<M: SyncModel>(&self, id: Uuid) -> Result<Option<M>, OrmError> {
        self.registry.typed::<M>()?;
        self.tx.get::<M>(id).await
    }
    pub async fn save<M: SyncModel>(&self, model: &M) -> Result<(), OrmError> {
        let d = self.registry.typed::<M>()?;
        let id = model.primary_key_value();
        save_local(self.tx, d, id, record(model)?).await
    }
    pub async fn delete<M: SyncModel>(&self, id: Uuid) -> Result<(), OrmError> {
        let d = self.registry.typed::<M>()?;
        delete_local(self.tx, self.registry, d.name, id).await
    }
}

async fn save_local(
    tx: &Transaction,
    d: &crate::model::Descriptor,
    id: Uuid,
    mut data: Record,
) -> Result<(), OrmError> {
    if let Some(previous) = read(tx, d, id, false).await? {
        for field in d.fields.iter().filter(|f| !f.writable) {
            data.insert(field.name.into(), previous[field.name].clone());
        }
    }
    write(tx, d, id, Some(&data), false).await?;
    set_deleting(tx, d.name, id, false).await?;
    mark(tx, d.name, id).await
}

fn delete_local<'a>(
    tx: &'a Transaction,
    registry: &'a Registry,
    name: &'a str,
    id: Uuid,
) -> TransactionFuture<'a, ()> {
    Box::pin(async move {
        let parent = registry.get(name)?;
        for child in registry.models.iter().rev() {
            for column in &child.schema.columns {
                let Some(fk) = &column.references else {
                    continue;
                };
                if fk.target_parts().map(|p| p.0) != Some(parent.schema.name) {
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
                        vec![che_orm::DatabaseValue::Uuid(id)],
                    )
                    .await?;
                for row in rows {
                    let child_id = row.get::<Uuid>(0)?;
                    match fk
                        .on_delete()
                        .unwrap_or("restrict")
                        .to_ascii_lowercase()
                        .as_str()
                    {
                        "cascade" => delete_local(tx, registry, child.name, child_id).await?,
                        "set null" => {
                            let mut record = read(tx, child, child_id, false).await?.unwrap();
                            let field = child
                                .fields
                                .iter()
                                .find(|f| f.column == column.name)
                                .unwrap();
                            if !field.writable {
                                return Err(invalid("SET NULL requires writable sync FK"));
                            }
                            record.insert(field.name.into(), Value::Null);
                            write(tx, child, child_id, Some(&record), false).await?;
                            mark(tx, child.name, child_id).await?;
                        }
                        _ => return Err(invalid("delete restricted by related records")),
                    }
                }
            }
        }
        write(tx, parent, id, None, false).await?;
        set_deleting(tx, name, id, true).await?;
        if let Some(mut conflict) = load_conflict(tx, name, id).await? {
            if conflict.retryable {
                // Deleting a deferred received row is an explicit choice, not
                // another automatic attempt to insert that row.
                tx.execute_sql(
                    "DELETE FROM _sync_conflicts WHERE model = $1 AND entity_id = $2",
                    vec![text(name), text(id)],
                )
                .await?;
            } else {
                conflict.local = None;
                store_conflict(tx, &conflict).await?;
            }
        }
        mark(tx, name, id).await
    })
}

fn validate_entity(d: &crate::model::Descriptor, entity: &Entity) -> Result<(), OrmError> {
    if entity.schema_version != d.version || entity.revision < 0 {
        return Err(invalid("incompatible entity version"));
    }
    if let Some(record) = &entity.record {
        d.validate(record, entity.id, true, false)?;
    }
    Ok(())
}

struct Update {
    acknowledgement: Option<Entity>,
    remote: Entity,
    merged: Option<Record>,
    conflict: Option<Conflict>,
    previous: Option<Record>,
    base: Option<Record>,
    /// Dependency protection can add local-only rows without inventing a
    /// server tombstone or revision for an unsent entity.
    received: bool,
}

async fn reconcile(
    tx: &Transaction,
    registry: &Registry,
    entities: Vec<Entity>,
    reset: bool,
) -> Result<Vec<Update>, OrmError> {
    let mut updates = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for remote in entities {
        if !seen.insert((remote.model.clone(), remote.id)) {
            return Err(invalid("duplicate entity in incoming group"));
        }
        let d = registry.get(&remote.model)?;
        validate_entity(d, &remote)?;
        let old = shadow(tx, d, remote.id).await?;
        let reset = reset && old.epoch != remote.epoch;
        let existing = load_conflict(tx, d.name, remote.id).await?;
        if !reset
            && (remote.revision < old.revision
                || (remote.revision == old.revision
                    && !existing.as_ref().is_some_and(|c| c.retryable)))
        {
            continue;
        }
        let local = read(tx, d, remote.id, false).await?;
        let deleted = deleting(tx, d.name, remote.id).await? && local.is_none();
        let base = if deleted {
            old.record
        } else if let Some(c) = existing.as_ref().filter(|c| c.retryable) {
            c.base.clone()
        } else {
            old.record
        };
        let acknowledgement = existing
            .as_ref()
            .filter(|c| c.retryable)
            .and_then(|c| c.acknowledgement.clone());
        let (merged, fields) = if deleted {
            merge::delete(base.as_ref(), remote.record.as_ref())
        } else if let Some(anchor) = &acknowledgement {
            let (effective, fields) =
                merge::acknowledge(d, base.as_ref(), local.as_ref(), anchor.record.as_ref());
            if fields.is_empty() && remote.revision != anchor.revision {
                merge::merge(
                    d,
                    anchor.record.as_ref(),
                    effective.as_ref(),
                    remote.record.as_ref(),
                )
            } else {
                (effective, fields)
            }
        } else {
            merge::merge(d, base.as_ref(), local.as_ref(), remote.record.as_ref())
        };
        let conflict = if let Some(mut c) = existing.filter(|c| !c.retryable) {
            c.remote = remote.clone();
            c.local = local.clone();
            Some(c)
        } else if !fields.is_empty() {
            Some(Conflict {
                model: d.name.into(),
                id: remote.id,
                base: acknowledgement
                    .as_ref()
                    .map(|e| e.record.clone())
                    .unwrap_or_else(|| base.clone()),
                local: local.clone(),
                remote: remote.clone(),
                fields,
                reason: None,
                retryable: false,
                acknowledgement: None,
            })
        } else {
            None
        };
        updates.push(Update {
            acknowledgement,
            remote,
            merged,
            conflict,
            previous: local,
            base,
            received: true,
        });
    }
    Ok(updates)
}

async fn retry_blocked(tx: &Transaction, registry: &Registry) -> Result<(), OrmError> {
    let rows = tx
        .query_sql("SELECT payload FROM _sync_conflicts", vec![])
        .await?;
    let mut entities = Vec::new();
    for row in rows {
        let c: Conflict = decode(&row.get::<String>(0)?)?;
        if c.retryable {
            entities.push(shadow(tx, registry.get(&c.model)?, c.id).await?);
        }
    }
    if !entities.is_empty() {
        let updates = reconcile(tx, registry, entities, false).await?;
        apply_updates(tx, registry, updates).await?;
    }
    Ok(())
}

async fn apply_updates(
    tx: &Transaction,
    registry: &Registry,
    mut updates: Vec<Update>,
) -> Result<(), OrmError> {
    protect_dependencies(tx, registry, &mut updates).await?;
    // Old shadows and local rows have already been captured in memory. Persist
    // authoritative state first, then reconcile the visible group atomically.
    for u in &updates {
        if u.received {
            set_shadow(tx, registry.get(&u.remote.model)?, &u.remote).await?;
        }
    }
    // Keep each incoming group atomic. A constraint failure rolls all visible
    // changes back, but retains remote shadows and durable conflict records.
    savepoint(tx).await?;
    let result = async {
        let targets = Arc::new(
            updates
                .iter()
                .map(|u| ((u.remote.model.clone(), u.remote.id), u.merged.clone()))
                .collect::<Targets>(),
        );
        let mut plan = Vec::new();
        for d in registry.models.iter() {
            for u in updates
                .iter()
                .filter(|u| u.remote.model == d.name && u.merged.is_some())
            {
                plan.push((d.name.to_owned(), u.remote.id, u.merged.clone()));
            }
        }
        for d in registry.models.iter().rev() {
            for u in updates
                .iter()
                .filter(|u| u.remote.model == d.name && u.merged.is_none())
            {
                plan.push((d.name.to_owned(), u.remote.id, None));
            }
        }
        let write_registry = registry.clone();
        let plan = Arc::new(plan);
        ordered(tx, plan.len(), move |tx, index| {
            let plan = plan.clone();
            let registry = write_registry.clone();
            let targets = targets.clone();
            Box::pin(async move {
                let (model, id, record) = &plan[index];
                let d = registry.get(model)?;
                if record.is_none() {
                    guard_delete(tx, &registry, d, *id, &targets).await?;
                }
                write(tx, d, *id, record.as_ref(), false).await
            })
        })
        .await?;
        // FK actions may silently delete or modify another intended merged row.
        for u in &updates {
            if read(tx, registry.get(&u.remote.model)?, u.remote.id, false).await? != u.merged {
                return Err(invalid(REFERENTIAL_CONFLICT));
            }
        }
        Ok::<_, OrmError>(())
    }
    .await;
    match result {
        Ok(()) => release(tx).await?,
        Err(error) if application_conflict(&error) => {
            rollback(tx).await?;
            for u in &mut updates {
                if let Some(conflict) = &mut u.conflict {
                    conflict.local = u.previous.clone();
                    if conflict.reason.is_none() {
                        conflict.reason = Some(error.to_string());
                    }
                } else {
                    u.conflict = Some(Conflict {
                        model: u.remote.model.clone(),
                        id: u.remote.id,
                        base: u.base.clone(),
                        local: u.previous.clone(),
                        remote: u.remote.clone(),
                        fields: vec![],
                        reason: Some(error.to_string()),
                        retryable: u.acknowledgement.is_some() || u.previous == u.base,
                        acknowledgement: u.acknowledgement.clone(),
                    });
                }
            }
        }
        Err(error) => {
            rollback(tx).await?;
            return Err(error);
        }
    }
    for u in updates {
        let d = registry.get(&u.remote.model)?;
        if let Some(conflict) = u.conflict {
            store_conflict(tx, &conflict).await?;
        } else {
            tx.execute_sql(
                "DELETE FROM _sync_conflicts WHERE model = $1 AND entity_id = $2",
                vec![text(d.name), text(u.remote.id)],
            )
            .await?;
        }
        if u.remote.record.is_none() && read(tx, d, u.remote.id, false).await?.is_none() {
            set_deleting(tx, d.name, u.remote.id, false).await?;
        }
        mark(tx, d.name, u.remote.id).await?;
    }
    Ok(())
}

async fn load_conflict(
    tx: &Transaction,
    model: &str,
    id: Uuid,
) -> Result<Option<Conflict>, OrmError> {
    let rows = tx
        .query_sql(
            "SELECT payload FROM _sync_conflicts WHERE model = $1 AND entity_id = $2",
            vec![text(model), text(id)],
        )
        .await?;
    rows.first()
        .map(|r| decode(&r.get::<String>(0)?))
        .transpose()
}

async fn store_conflict(tx: &Transaction, conflict: &Conflict) -> Result<(), OrmError> {
    tx.execute_sql("INSERT INTO _sync_conflicts (model, entity_id, payload) VALUES ($1,$2,$3) ON CONFLICT (model, entity_id) DO UPDATE SET payload = excluded.payload", vec![text(&conflict.model), text(conflict.id), text(encode(conflict)?)]).await?;
    Ok(())
}

async fn protect_dependencies(
    tx: &Transaction,
    registry: &Registry,
    updates: &mut Vec<Update>,
) -> Result<(), OrmError> {
    let mut queue: Vec<_> = updates
        .iter()
        .filter(|u| u.merged.is_none())
        .map(|u| (u.remote.model.clone(), u.remote.id))
        .collect();
    let mut visited = std::collections::HashSet::new();
    while let Some((model, id)) = queue.pop() {
        if !visited.insert((model.clone(), id)) {
            continue;
        }
        for child in dependents(tx, registry, registry.get(&model)?, id).await? {
            let index = if let Some(index) = updates
                .iter()
                .position(|u| u.remote.model == child.model && u.remote.id == child.id)
            {
                index
            } else {
                let remote = shadow(tx, registry.get(&child.model)?, child.id).await?;
                // The child may already have a field, rejection or deletion
                // conflict. Its original base is no longer in shadow after a
                // tombstone, so never replace it with a synthetic snapshot.
                let mut conflict = load_conflict(tx, &child.model, child.id)
                    .await?
                    .unwrap_or_else(|| Conflict {
                        model: child.model.clone(),
                        id: child.id,
                        base: remote.record.clone(),
                        local: Some(child.record.clone()),
                        remote: remote.clone(),
                        fields: vec!["$entity".into()],
                        reason: Some(REFERENTIAL_CONFLICT.into()),
                        retryable: false,
                        acknowledgement: None,
                    });
                conflict.local = Some(child.record.clone());
                conflict.remote = remote.clone();
                if conflict.reason.is_none() {
                    conflict.reason = Some(REFERENTIAL_CONFLICT.into());
                }
                updates.push(Update {
                    acknowledgement: conflict.acknowledgement.clone(),
                    base: conflict.base.clone(),
                    remote,
                    merged: Some(child.record.clone()),
                    conflict: Some(conflict),
                    previous: Some(child.record),
                    received: false,
                });
                updates.len() - 1
            };
            if child.cascade
                && (updates[index].merged.is_none()
                    || updates[index]
                        .merged
                        .as_ref()
                        .and_then(|r| r.get(child.field))
                        == Some(&Value::Uuid(id)))
            {
                queue.push((child.model, child.id));
            }
        }
    }
    Ok(())
}
