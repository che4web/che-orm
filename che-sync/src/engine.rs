use crate::{SyncClient, SyncServer, invalid, protocol::*};
use che_orm::OrmError;
use std::{future::Future, pin::Pin};

pub type TransportFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, OrmError>> + Send + 'a>>;

/// Implement with an authenticated HTTP adapter, IPC, or an in-process server.
/// A transport error leaves the durable operation intact for a later retry.
pub trait SyncTransport: Send + Sync {
    fn push(&self, request: PushRequest) -> TransportFuture<'_, PushResult>;
    fn pull(&self, request: PullRequest) -> TransportFuture<'_, PullResult>;
    fn snapshot(&self) -> TransportFuture<'_, Snapshot>;
}

impl SyncTransport for SyncServer {
    fn push(&self, request: PushRequest) -> TransportFuture<'_, PushResult> {
        Box::pin(SyncServer::push(self, request))
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
}

pub struct SyncEngine<T> {
    client: SyncClient,
    transport: T,
    page_size: u32,
}

impl<T: SyncTransport> SyncEngine<T> {
    pub fn new(client: SyncClient, transport: T) -> Self {
        Self {
            client,
            transport,
            page_size: 100,
        }
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

    /// Push-first: retry or prepare one immutable operation, process its result,
    /// then receive one page (or the initial snapshot). A failed push prevents
    /// pull from replacing the shadow base of that operation.
    /// Offline writes may continue while transport requests are in flight.
    pub async fn sync_once(&self) -> Result<SyncReport, OrmError> {
        let _guard = self.client.cycle.lock().await;
        let mut report = SyncReport::default();
        let pending = match self.client.pending().await? {
            Some(pending) => Some(pending),
            None => self.client.prepare().await?,
        };
        if let Some(pending) = pending {
            let result = self.transport.push(pending.request.clone()).await?;
            count(&mut report, &result);
            self.client.finish(pending, result).await?;
        }
        self.receive(&mut report).await?;
        report.conflicts = self.client.conflicts().await?.len();
        // Determine remaining actionable local work without mutating outbox.
        let dirty = self
            .client
            .database()
            .query_sql("SELECT model FROM _sync_dirty LIMIT 1", vec![])
            .await?;
        report.has_more |= !dirty.is_empty() && report.conflicts == 0;
        Ok(report)
    }

    async fn receive(&self, report: &mut SyncReport) -> Result<(), OrmError> {
        let cursor = self.client.cursor().await?;
        if let Some(cursor) = cursor {
            match self
                .transport
                .pull(PullRequest {
                    protocol_version: PROTOCOL_VERSION,
                    cursor: cursor.clone(),
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
                    self.client.apply(groups, next, false).await?;
                    report.has_more = has_more;
                    return Ok(());
                }
                PullResult::ResetRequired => {}
            }
        }
        let snapshot = self.transport.snapshot().await?;
        report.received_changes += snapshot.entities.len();
        self.client
            .apply(
                vec![ChangeGroup {
                    sequence: snapshot.cursor.sequence,
                    entities: snapshot.entities,
                }],
                snapshot.cursor,
                true,
            )
            .await?;
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
