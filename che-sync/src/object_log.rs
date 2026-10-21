//! Opt-in object delivery. Reconciliation is transactional and payload-free at rest.
//! Every page rechecks current access; old events never grant historical data.
pub use crate::object_client::ObjectStatus;
pub use crate::protocol::object_log::*;
use crate::{
    Registry, SchemaRole, SyncModel, SyncServer, encode, invalid,
    storage::{quote, text},
};
use che_orm::{DatabaseValue as D, Model, OrmError, Transaction, TransactionFuture, Uuid};
use std::sync::Arc;

/// Application-owned ACL evaluation. No HTTP or group schema belongs in sync.
pub trait ObjectAccessPolicy: Send + Sync + 'static {
    /// Stable server-owned projection identity, e.g. the REST resource path.
    /// Distinct projections of one model must not share delivery cursors.
    fn feed_namespace(&self) -> &str {
        ""
    }
    fn rights<'a>(
        &'a self,
        tx: &'a Transaction,
        model: &'a str,
        id: Uuid,
    ) -> TransactionFuture<'a, ObjectRights>;
    fn can_create<'a>(&'a self, tx: &'a Transaction, model: &'a str)
    -> TransactionFuture<'a, bool>;
}

#[derive(Debug, Model)]
#[orm(table = "_sync_object_feeds", unique("principal", "model"))]
struct Feed {
    #[orm(primary_key)]
    id: Uuid,
    principal: String,
    model: String,
    history: String,
    sequence: i64,
}
#[derive(Debug, Model)]
#[orm(table = "_sync_object_visibility", unique("feed", "entity_id"))]
struct Visibility {
    #[orm(primary_key)]
    id: i64,
    feed: String,
    entity_id: Uuid,
    revision: i64,
    rights: String,
    status: String,
}
#[derive(Debug, Model)]
#[orm(table = "_sync_object_events", unique("feed", "sequence"))]
struct Delivery {
    #[orm(primary_key)]
    id: i64,
    feed: String,
    sequence: i64,
    entity_id: Uuid,
}
#[derive(Debug, Model)]
#[orm(table = "_sync_object_access", unique("model", "entity_id"))]
struct LocalAccess {
    #[orm(primary_key)]
    id: i64,
    model: String,
    entity_id: Uuid,
    readable: bool,
    changeable: bool,
    deletable: bool,
    status: String,
}
#[derive(Debug, Model)]
#[orm(table = "_sync_object_models")]
struct LocalModel {
    #[orm(primary_key)]
    id: i64,
    #[orm(unique)]
    model: String,
    can_create: bool,
}

/// Freeze these operations in a *follow-up* compiled application migration.
/// Never append to a previously applied migration or create tables at open/GET.
pub fn migration_operations(role: SchemaRole) -> Vec<che_orm::MigrationOperation> {
    let schema = schema(role);
    che_orm::ProjectState::from_app_schemas([("che_object_sync", &schema)])
        .expect("static object-delivery schema")
        .models()
        .iter()
        .cloned()
        .map(|model| che_orm::MigrationOperation::CreateModel { model })
        .collect()
}

/// Declare under the stable application name `che_object_sync` so subsequent
/// migration discovery retains the same infrastructure table identities.
pub fn schema(role: SchemaRole) -> che_orm::SchemaSet {
    match role {
        SchemaRole::Server => che_orm::SchemaSet::new()
            .model::<Feed>()
            .model::<Visibility>()
            .model::<Delivery>(),
        SchemaRole::Client => che_orm::SchemaSet::new()
            .model::<LocalAccess>()
            .model::<LocalModel>(),
    }
}

/// Relation projections require a separate, explicit contract. Reject them now.
pub fn validate_registry(registry: &Registry) -> Result<(), OrmError> {
    if registry
        .models
        .iter()
        .any(|d| d.schema.columns.iter().any(|c| c.references.is_some()))
    {
        return Err(invalid(
            "object delivery currently requires models without foreign keys",
        ));
    }
    Ok(())
}

impl SyncServer {
    /// Reconcile current rights and revisions with this principal's durable feed.
    /// Scans all model rows and known objects per call, deliberately synchronous.
    /// Serializing on journal rows coordinates all ordinary captured data writes.
    pub async fn object_changes<M: SyncModel>(
        &self,
        cursor: Option<ObjectCursor>,
        limit: u32,
        policy: Arc<dyn ObjectAccessPolicy>,
    ) -> Result<ObjectChanges, OrmError> {
        self.policy.authorize_scope(&self.principal, &self.scope)?;
        validate_registry(&self.registry)?;
        self.registry.check_model::<M>()?;
        if !self.model_logs || self.model.as_ref().is_some_and(|m| m != M::SYNC_NAME) {
            return Err(invalid(
                "object delivery is outside this configured model feed",
            ));
        }
        if self.scope != crate::model_log::MODEL_LOG_SCOPE || !(1..=1000).contains(&limit) {
            return Err(invalid(
                "object delivery needs model journals and a limit of 1..=1000",
            ));
        }
        let server = self.clone();
        self.db
            .transaction(move |tx| Box::pin(reconcile_feed::<M>(tx, server, cursor, limit, policy)))
            .await
    }

    /// Disclosure filtering never mutates the stored receipt. Recheck current
    /// rights for every canonical object, including accepted create/delete replies.
    pub async fn object_write_result(
        &self,
        operation_id: Uuid,
        result: crate::protocol::PushResult,
        policy: Arc<dyn ObjectAccessPolicy>,
    ) -> Result<ObjectWriteResult, OrmError> {
        validate_registry(&self.registry)?;
        if !self.model_logs {
            return Err(invalid("object receipts require model journals"));
        }
        self.policy.authorize_scope(&self.principal, &self.scope)?;
        let server = self.clone();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    crate::model_log::lock_logs(tx, &server.registry).await?;
                    let (outcome, entities) = match &result {
                        crate::protocol::PushResult::Accepted(e) => {
                            (WriteOutcome::Accepted, e.as_slice())
                        }
                        crate::protocol::PushResult::Conflict(e) => {
                            (WriteOutcome::Conflict, e.as_slice())
                        }
                        crate::protocol::PushResult::Rejected { .. } => {
                            (WriteOutcome::Rejected, &[][..])
                        }
                    };
                    for e in entities {
                        let current = server
                            .entity(tx, server.registry.get(&e.model)?, e.id)
                            .await?;
                        // A newly granted read authorizes the current state, not
                        // business fields retained in an older immutable receipt.
                        if !policy.rights(tx, &e.model, e.id).await?.read
                            || current.revision != e.revision
                            || current.record != e.record
                        {
                            return Ok(ObjectWriteResult::Acknowledged {
                                operation_id,
                                outcome,
                            });
                        }
                    }
                    Ok(ObjectWriteResult::Canonical(
                        server.registry.model_result(result)?,
                    ))
                })
            })
            .await
    }
}

async fn reconcile_feed<M: SyncModel>(
    tx: &Transaction,
    server: SyncServer,
    cursor: Option<ObjectCursor>,
    limit: u32,
    policy: Arc<dyn ObjectAccessPolicy>,
) -> Result<ObjectChanges, OrmError> {
    server
        .policy
        .authorize_scope(&server.principal, &server.scope)?;
    crate::model_log::lock_logs(tx, &server.registry).await?;
    crate::model_log::require_baselines(tx, &server.registry).await?;
    let d = server.registry.typed::<M>()?;
    let rows = tx
        .query_sql(
            format!(
                "SELECT history FROM {} WHERE key = 0",
                quote(&d.log_state_table())
            ),
            vec![],
        )
        .await?;
    let history: Uuid = rows[0]
        .get::<String>(0)?
        .parse()
        .map_err(|_| invalid("invalid history"))?;
    let feed_model = encode(&(d.name, policy.feed_namespace()))?;
    tx.execute_sql("INSERT INTO _sync_object_feeds (id,principal,model,history,sequence) VALUES ($1,$2,$3,$4,0) ON CONFLICT (principal,model) DO NOTHING",vec![D::Uuid(Uuid::now_v7()),text(&server.principal),text(&feed_model),text(history)]).await?;
    let feed = tx
        .fetch_one(
            Feed::query().filter(
                Feed::PRINCIPAL
                    .eq(server.principal.as_str())
                    .and(Feed::MODEL.eq(feed_model.as_str())),
            ),
        )
        .await?
        .ok_or_else(|| invalid("missing delivery feed"))?;
    if feed.history != history.to_string() {
        return Err(invalid("object delivery history changed"));
    }
    let after = if let Some(c) = &cursor {
        if c.model != d.name
            || c.history != history
            || c.feed != feed.id
            || c.sequence < 0
            || c.sequence > feed.sequence
        {
            return Err(invalid("cursor belongs to another object feed or boundary"));
        }
        c.sequence
    } else {
        0
    };
    let feed_key = feed.id.to_string();
    let mut sequence = feed.sequence;
    let known = tx
        .fetch_all(Visibility::query().filter(Visibility::FEED.eq(feed_key.as_str())))
        .await?
        .into_iter()
        .map(|v| (v.entity_id, v))
        .collect::<std::collections::BTreeMap<_, _>>();
    let rows = tx
        .query_sql(
            format!(
                "SELECT {} FROM {} ORDER BY {}",
                quote(d.primary),
                quote(d.schema.name),
                quote(d.primary)
            ),
            vec![],
        )
        .await?;
    let mut ids = rows
        .iter()
        .map(|r| r.get::<Uuid>(0).map_err(OrmError::from))
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    ids.extend(known.keys().copied());
    for id in ids {
        let entity = server.entity(tx, d, id).await?;
        let rights = policy.rights(tx, d.name, id).await?;
        let old = known.get(&id);
        let status = if !rights.read {
            "revoked"
        } else if entity.record.is_none() {
            "deleted"
        } else {
            "active"
        };
        // No identity, revision, or deletion information for never-visible objects.
        if old.is_none() && status != "active" {
            continue;
        }
        let revision = if status == "revoked" {
            0
        } else {
            entity.revision
        };
        let rights = if status == "active" {
            rights
        } else {
            ObjectRights::default()
        };
        let encoded_rights = encode(&rights)?;
        if old.is_some_and(|v| {
            v.revision == revision && v.rights == encoded_rights && v.status == status
        }) {
            continue;
        }
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| invalid("delivery sequence overflow"))?;
        tx.execute_sql("INSERT INTO _sync_object_visibility (feed,entity_id,revision,rights,status) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (feed,entity_id) DO UPDATE SET revision=excluded.revision,rights=excluded.rights,status=excluded.status",vec![text(&feed_key),D::Uuid(id),D::Integer(revision),text(encoded_rights),text(status)]).await?;
        tx.execute_sql(
            "INSERT INTO _sync_object_events (feed,sequence,entity_id) VALUES ($1,$2,$3)",
            vec![text(&feed_key), D::Integer(sequence), D::Uuid(id)],
        )
        .await?;
    }
    tx.execute_sql(
        "UPDATE _sync_object_feeds SET sequence=$1 WHERE id=$2",
        vec![D::Integer(sequence), D::Uuid(feed.id)],
    )
    .await?;
    let deliveries = tx.query_sql("SELECT sequence,entity_id FROM _sync_object_events WHERE feed=$1 AND sequence>$2 ORDER BY sequence LIMIT $3",vec![text(&feed_key),D::Integer(after),D::Integer(i64::from(limit))]).await?;
    let mut events = Vec::new();
    let mut next = after;
    for row in deliveries {
        next = row.get(0)?;
        let id = row.get(1)?;
        // Re-evaluate even replayed pages: an old grant cannot leak after revoke.
        let rights = policy.rights(tx, d.name, id).await?;
        let change = if !rights.read {
            ObjectChange::Revoked { record_id: id }
        } else {
            let entity = server.entity(tx, d, id).await?;
            if entity.record.is_none() {
                ObjectChange::Deleted {
                    record_id: id,
                    revision: entity.revision,
                }
            } else {
                ObjectChange::State {
                    state: server.registry.model_state(&entity)?,
                    rights,
                }
            }
        };
        events.push(ObjectEvent {
            sequence: next,
            change,
        });
    }
    Ok(ObjectChanges {
        protocol_version: OBJECT_LOG_PROTOCOL,
        cursor: ObjectCursor {
            model: d.name.into(),
            history,
            feed: feed.id,
            sequence: next,
        },
        after,
        events,
        can_create: policy.can_create(tx, d.name).await?,
        has_more: next < sequence,
    })
}
