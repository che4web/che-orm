use crate::{SyncClient, SyncServer, invalid, protocol::*};
use che_orm::OrmError;
use std::{future::Future, pin::Pin};

pub type TransportFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, OrmError>> + Send + 'a>>;

/// Implement with an authenticated HTTP adapter, IPC, or an in-process server.
/// A transport error leaves the durable operation intact for a later retry.
pub trait SyncTransport: Send + Sync {
    /// REST transports send one record per request; native transports may batch.
    fn max_mutations(&self) -> usize {
        usize::MAX
    }
    fn push(&self, request: PushRequest) -> TransportFuture<'_, PushResult>;
    /// Route a dependency to its own model endpoint. Single-model transports
    /// may use the default; multi-model HTTP adapters must override this.
    fn push_model(&self, _model: &str, request: PushRequest) -> TransportFuture<'_, PushResult> {
        self.push(request)
    }
    fn pull(&self, request: PullRequest) -> TransportFuture<'_, PullResult>;
    fn snapshot(&self) -> TransportFuture<'_, Snapshot>;
    /// One full-load implementation serves initial loading and journal resets.
    fn full_load(&self) -> TransportFuture<'_, Snapshot> {
        self.snapshot()
    }
}

impl SyncTransport for SyncServer {
    fn push(&self, request: PushRequest) -> TransportFuture<'_, PushResult> {
        Box::pin(SyncServer::push(self, request))
    }
    fn push_model(&self, model: &str, request: PushRequest) -> TransportFuture<'_, PushResult> {
        let server = self.for_model_name(model);
        Box::pin(async move { server?.push(request).await })
    }
    fn pull(&self, request: PullRequest) -> TransportFuture<'_, PullResult> {
        Box::pin(SyncServer::pull(self, request))
    }
    fn snapshot(&self) -> TransportFuture<'_, Snapshot> {
        Box::pin(SyncServer::snapshot(self))
    }
}

#[derive(Debug, Clone, Default)]
pub struct SyncReport {
    pub accepted_operations: usize,
    pub rejected_operations: usize,
    pub received_changes: usize,
    pub conflicts: usize,
    pub has_more: bool,
    /// A related immutable operation or conflict needs completion/resolution.
    pub blocked: bool,
}

pub struct SyncEngine<T> {
    client: SyncClient,
    transport: T,
    page_size: u32,
    model: Option<String>,
}

impl<T: SyncTransport> SyncEngine<T> {
    pub fn new(client: SyncClient, transport: T) -> Self {
        Self {
            client,
            transport,
            page_size: 100,
            model: None,
        }
    }
    /// Independently synchronize one model. The transport must be bound to the
    /// same model; required FK creates are added to its outgoing group.
    pub fn for_model<M: crate::SyncModel>(
        client: SyncClient,
        transport: T,
    ) -> Result<Self, OrmError> {
        client.registry.check_model::<M>()?;
        Ok(Self {
            client,
            transport,
            page_size: 100,
            model: Some(M::SYNC_NAME.into()),
        })
    }
    pub fn client(&self) -> &SyncClient {
        &self.client
    }
    pub fn with_page_size(mut self, size: u32) -> Result<Self, OrmError> {
        if size == 0 || size > 1000 {
            return Err(invalid("page size must be 1..=1000"));
        }
        self.page_size = size;
        Ok(self)
    }

    /// Finish an unknown outcome, then pull/merge before preparing a new patch.
    /// Offline writes may continue while transport requests are in flight.
    pub async fn sync_once(&self) -> Result<SyncReport, OrmError> {
        let cycle = self
            .client
            .cycles
            .lock()
            .await
            .entry(self.model.clone())
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = cycle.lock().await;
        let mut report = SyncReport::default();
        let pending = self.client.pending(self.model.as_deref()).await?;
        if let Some(pending) = pending {
            self.send_pending(pending, &mut report).await?;
        }
        self.receive(&mut report).await?;
        if !report.blocked && !report.has_more {
            if let Some(pending) = self
                .client
                .prepare(self.model.as_deref(), self.transport.max_mutations())
                .await?
            {
                self.send_pending(pending, &mut report).await?;
                report.has_more = true;
            }
        }
        let (conflicts, dirty, blocked) = self.client.feed_status(self.model.as_deref()).await?;
        report.conflicts = conflicts;
        report.blocked |= blocked || (dirty && conflicts > 0);
        report.has_more |= dirty && conflicts == 0 && !report.blocked;
        Ok(report)
    }

    async fn send_pending(
        &self,
        pending: crate::client::Pending,
        report: &mut SyncReport,
    ) -> Result<(), OrmError> {
        let id = pending.request.operation.id;
        let operation = {
            let mut operations = self.client.operations.lock().await;
            operations.retain(|_, lock| std::sync::Arc::strong_count(lock) > 1);
            operations
                .entry(id)
                .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _operation = operation.lock().await;
        // Another model may have completed this required parent operation
        // while this cycle was waiting for its operation-specific lock.
        if !self
            .client
            .database()
            .query_sql(
                "SELECT operation_id FROM _sync_outbox WHERE operation_id = $1",
                vec![che_orm::DatabaseValue::Text(id.to_string())],
            )
            .await?
            .is_empty()
        {
            let result = match pending.model.as_deref() {
                Some(model) => {
                    self.transport
                        .push_model(model, pending.request.clone())
                        .await?
                }
                None => self.transport.push(pending.request.clone()).await?,
            };
            count(report, &result);
            self.client.finish(pending, result).await?;
        }
        Ok(())
    }

    async fn receive(&self, report: &mut SyncReport) -> Result<(), OrmError> {
        let cursor = self.client.cursor(self.model.as_deref()).await?;
        if let Some(cursor) = cursor {
            match self
                .transport
                .pull(PullRequest {
                    protocol_version: PROTOCOL_VERSION,
                    cursor: Some(cursor.clone()),
                    limit: self.page_size,
                })
                .await?
            {
                PullResult::Changes {
                    groups,
                    cursor: next,
                    has_more,
                } => {
                    if next.scope != cursor.scope
                        || next.model != cursor.model
                        || next.epoch != cursor.epoch
                        || next.sequence < cursor.sequence
                    {
                        return Err(invalid("invalid pull cursor"));
                    }
                    let mut expected = cursor.sequence;
                    for group in &groups {
                        expected = expected
                            .checked_add(1)
                            .ok_or_else(|| invalid("cursor overflow"))?;
                        if group.sequence != expected {
                            return Err(invalid("non-contiguous pull page"));
                        }
                    }
                    if next.sequence != expected || (has_more && expected == cursor.sequence) {
                        return Err(invalid("pull cursor does not match page"));
                    }
                    report.received_changes +=
                        groups.iter().map(|g| g.entities.len()).sum::<usize>();
                    if !self.client.apply(groups, next, false).await? {
                        report.blocked = true;
                    }
                    report.has_more = has_more;
                    return Ok(());
                }
                PullResult::ResetRequired => {}
                PullResult::Full(_) => {
                    return Err(invalid("unexpected full response to incremental pull"));
                }
            }
        }
        let snapshot = self.transport.full_load().await?;
        if snapshot.cursor.model != self.model {
            return Err(invalid("snapshot belongs to another model feed"));
        }
        report.received_changes += snapshot.entities.len();
        if !self
            .client
            .apply(
                vec![ChangeGroup {
                    sequence: snapshot.cursor.sequence,
                    entities: snapshot.entities,
                }],
                snapshot.cursor,
                true,
            )
            .await?
        {
            report.blocked = true;
        }
        report.has_more = false;
        Ok(())
    }
}

fn count(report: &mut SyncReport, result: &PushResult) {
    match result {
        PushResult::Accepted(_) => report.accepted_operations += 1,
        PushResult::Rejected { .. } => report.rejected_operations += 1,
        PushResult::Conflict(_) => {}
    }
}
