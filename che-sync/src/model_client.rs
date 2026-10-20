//! Tauri-friendly current/confirmed storage and immutable ordinary REST writes.
use crate::{
    Registry, Resolution, SyncClient, SyncModel, SyncReport, TransportFuture, invalid,
    model_log::*,
    protocol::PushResult,
    storage::{meta, set_meta, text},
};
use che_orm::{Database, OrmError, Uuid};

pub trait ModelLogTransport: Send + Sync {
    /// Bind serializer names and resource path once, before persisting a request.
    fn encode_write(&self, write: RestModelWrite) -> Result<RestModelWrite, OrmError> {
        Ok(write)
    }
    fn write<'a>(&'a self, write: &'a RestModelWrite) -> TransportFuture<'a, ModelWriteResult>;
    fn changes<'a>(
        &'a self,
        model: &'a str,
        after: i64,
        limit: u32,
    ) -> TransportFuture<'a, ModelChanges>;
}

#[derive(Clone)]
pub struct ModelLogClient {
    inner: SyncClient,
}
impl ModelLogClient {
    pub async fn open(db: Database, registry: Registry) -> Result<Self, OrmError> {
        let inner =
            SyncClient::open_with_protocol(db, registry, MODEL_LOG_SCOPE.into(), true).await?;
        Ok(Self { inner })
    }

    pub fn database(&self) -> &Database {
        self.inner.database()
    }
    pub async fn save<M: SyncModel>(&self, model: &M) -> Result<(), OrmError> {
        self.inner.save(model).await
    }
    pub async fn delete<M: SyncModel>(&self, id: Uuid) -> Result<(), OrmError> {
        self.inner.delete::<M>(id).await
    }
    pub async fn conflicts(&self) -> Result<Vec<crate::protocol::Conflict>, OrmError> {
        self.inner.conflicts().await
    }
    pub async fn resolve(
        &self,
        model: &str,
        id: Uuid,
        resolution: Resolution,
    ) -> Result<(), OrmError> {
        self.inner.resolve(model, id, resolution).await
    }
    pub async fn cursor<M: SyncModel>(&self) -> Result<i64, OrmError> {
        Ok(self
            .inner
            .cursor(Some(M::SYNC_NAME))
            .await?
            .map_or(0, |cursor| cursor.sequence))
    }
    pub async fn apply(&self, changes: ModelChanges) -> Result<bool, OrmError> {
        self.inner.apply_model_changes(changes).await
    }
    pub fn engine<T: ModelLogTransport>(&self, transport: T) -> ModelLogEngine<T> {
        ModelLogEngine {
            client: self.clone(),
            transport,
            page_size: 100,
        }
    }
}

pub struct ModelLogEngine<T> {
    client: ModelLogClient,
    transport: T,
    page_size: u32,
}
impl<T: ModelLogTransport> ModelLogEngine<T> {
    pub fn with_page_size(mut self, page_size: u32) -> Result<Self, OrmError> {
        if !(1..=1000).contains(&page_size) {
            return Err(invalid("page size must be 1..=1000"));
        }
        self.page_size = page_size;
        Ok(self)
    }
    pub fn client(&self) -> &ModelLogClient {
        &self.client
    }
    /// Download one page of each model in parent-first registration order.
    pub async fn pull(&self) -> Result<SyncReport, OrmError> {
        let cycle = self
            .client
            .inner
            .cycles
            .lock()
            .await
            .entry(None)
            .or_default()
            .clone();
        let _guard = cycle.lock().await;
        self.pull_inner().await
    }
    async fn pull_inner(&self) -> Result<SyncReport, OrmError> {
        let mut report = SyncReport::default();
        for d in self.client.inner.registry.models.iter() {
            let after = self
                .client
                .inner
                .cursor(Some(d.name))
                .await?
                .map_or(0, |cursor| cursor.sequence);
            let page = self
                .transport
                .changes(d.name, after, self.page_size)
                .await?;
            if page.model != d.name {
                return Err(invalid("journal belongs to another model"));
            }
            let more = page.has_more;
            let received = page.operations.len();
            if self.client.apply(page).await? {
                report.received_changes += received;
                report.has_more |= more;
            } else {
                report.blocked = true;
            }
        }
        report.conflicts = self.client.conflicts().await?.len();
        Ok(report)
    }
    /// Send one coalesced object per model. Already submitted requests are always
    /// retried first, retaining the exact persisted resource/body/identity.
    pub async fn flush(&self) -> Result<SyncReport, OrmError> {
        let cycle = self
            .client
            .inner
            .cycles
            .lock()
            .await
            .entry(None)
            .or_default()
            .clone();
        let _guard = cycle.lock().await;
        self.flush_inner(true).await
    }
    async fn flush_inner(&self, prepare_new: bool) -> Result<SyncReport, OrmError> {
        let mut report = SyncReport::default();
        for d in self.client.inner.registry.models.iter() {
            let pending = if let Some(pending) = self.client.inner.pending(Some(d.name)).await? {
                Some(pending)
            } else if prepare_new && self.client.inner.cursor(Some(d.name)).await?.is_some() {
                self.client.inner.prepare(Some(d.name), 1).await?
            } else {
                None
            };
            let Some(pending) = pending else { continue };
            let id = pending.request.operation.id;
            let lock = self
                .client
                .inner
                .operations
                .lock()
                .await
                .entry(id)
                .or_default()
                .clone();
            let _operation = lock.lock().await;
            let key = format!("rest-request:{id}");
            let existing = self
                .client
                .inner
                .db
                .transaction({
                    let key = key.clone();
                    move |tx| Box::pin(async move { meta::<RestModelWrite>(tx, &key).await })
                })
                .await?;
            let write = if let Some(write) = existing {
                write
            } else {
                let mutation = &pending.request.operation.mutations[0];
                let descriptor = self.client.inner.registry.get(&mutation.model)?;
                let body = mutation
                    .changes
                    .as_ref()
                    .map(|changes| {
                        changes
                            .iter()
                            .map(|(name, value)| {
                                Ok((name.clone(), descriptor.json_value(name, value)?))
                            })
                            .collect::<Result<serde_json::Map<_, _>, OrmError>>()
                    })
                    .transpose()?
                    .map(serde_json::Value::Object)
                    .unwrap_or(serde_json::Value::Null);
                let kind = if mutation.changes.is_none() {
                    ChangeKind::Delete
                } else if mutation.base_revision == 0 {
                    ChangeKind::Create
                } else {
                    ChangeKind::Update
                };
                let json_null_fields = mutation.changes.as_ref().map(|changes| changes.iter().filter(|(name, value)| descriptor.field(name).is_ok_and(|field| descriptor.json_columns.contains(&field.column)) && matches!(value, crate::protocol::Value::Text(value) if value == "null")).map(|(name, _)| name.clone()).collect()).unwrap_or_default();
                let write = self.transport.encode_write(RestModelWrite {
                    model: mutation.model.clone(),
                    resource: mutation.model.clone(),
                    record_id: mutation.id,
                    operation_id: id,
                    replica_id: pending.request.replica_id,
                    base_revision: mutation.base_revision,
                    schema_version: mutation.schema_version,
                    kind,
                    body,
                    json_null_fields,
                })?;
                if write.operation_id != id
                    || write.record_id != mutation.id
                    || write.base_revision != mutation.base_revision
                    || write.kind != kind
                    || write.model != mutation.model
                {
                    return Err(invalid("REST encoder changed operation identity"));
                }
                self.client
                    .inner
                    .db
                    .transaction({
                        let key = key.clone();
                        let write = write.clone();
                        move |tx| Box::pin(async move { set_meta(tx, &key, &write).await })
                    })
                    .await?;
                write
            };
            let result = self.transport.write(&write).await?;
            let result = match result {
                ModelWriteResult::Accepted(states) | ModelWriteResult::Conflict(states)
                    if states.is_empty() =>
                {
                    return Err(invalid("empty canonical write result"));
                }
                ModelWriteResult::Accepted(states) => {
                    report.accepted_operations += 1;
                    PushResult::Accepted(self.entities(states).await?)
                }
                ModelWriteResult::Conflict(states) => {
                    PushResult::Conflict(self.entities(states).await?)
                }
                ModelWriteResult::Rejected { reason, .. } => {
                    report.rejected_operations += 1;
                    PushResult::Rejected { reason }
                }
            };
            self.client
                .inner
                .finish_result(pending, result, true)
                .await?;
            self.client
                .inner
                .db
                .execute_sql("DELETE FROM _sync_meta WHERE key = $1", vec![text(key)])
                .await?;
        }
        let (conflicts, dirty, blocked) = self.client.inner.feed_status(None).await?;
        report.conflicts = conflicts;
        report.blocked = blocked || conflicts > 0;
        report.has_more = dirty && !report.blocked;
        Ok(report)
    }
    async fn entities(
        &self,
        states: Vec<ModelState>,
    ) -> Result<Vec<crate::protocol::Entity>, OrmError> {
        let mut entities = Vec::new();
        for state in states {
            let history = self
                .client
                .inner
                .cursor(Some(&state.model))
                .await?
                .map(|cursor| cursor.epoch);
            entities.push(self.client.inner.registry.model_entity(&state, history)?);
        }
        Ok(entities)
    }
    pub async fn sync_once(&self) -> Result<SyncReport, OrmError> {
        let cycle = self
            .client
            .inner
            .cycles
            .lock()
            .await
            .entry(None)
            .or_default()
            .clone();
        let _guard = cycle.lock().await;
        let settled = self.flush_inner(false).await?;
        let mut report = self.pull_inner().await?;
        if !report.has_more && !report.blocked {
            let sent = self.flush_inner(true).await?;
            report.accepted_operations += sent.accepted_operations;
            report.rejected_operations += sent.rejected_operations;
            report.has_more |= sent.has_more;
            report.blocked |= sent.blocked;
            report.conflicts = sent.conflicts;
        }
        report.accepted_operations += settled.accepted_operations;
        report.rejected_operations += settled.rejected_operations;
        Ok(report)
    }
}
