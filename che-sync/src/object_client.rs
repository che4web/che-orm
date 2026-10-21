//! Protocol-3 client access projection and retained offline drafts.
use crate::{
    SyncClient, client::Pending, decode, invalid, object_log::*, protocol::Cursor, storage::*,
};
use che_orm::{
    ColumnRef, DatabaseValue as D, Expr, OrmError, QueryAst, SelectAst, TableRef, Transaction, Uuid,
};
use std::collections::BTreeMap;

pub(crate) fn prepare_read(registry: &crate::Registry, mut ast: QueryAst) -> QueryAst {
    fn filter(d: &crate::model::Descriptor, table: &'static str) -> Expr {
        let sub = |readable: bool| SelectAst {
            table: TableRef::new("_sync_object_access"),
            columns: vec![ColumnRef::new("_sync_object_access", "entity_id")],
            joins: vec![],
            filter: Some(
                eq("_sync_object_access", "model", text(d.name)).and(if readable {
                    eq("_sync_object_access", "readable", D::Boolean(true))
                } else {
                    Expr::Value(D::Boolean(true))
                }),
            ),
            order_by: vec![],
            limit: None,
            offset: None,
        };
        let membership = |q| Expr::InQuery {
            left: Box::new(Expr::Column(ColumnRef::new(table, d.primary))),
            query: Box::new(q),
        };
        membership(sub(true))
            .or(eq(table, d.revision, D::Integer(0)).and(membership(sub(false)).not()))
    }
    if let QueryAst::Select(q) = &mut ast {
        if let Some(d) = registry
            .models
            .iter()
            .find(|d| d.schema.name == q.table.name)
        {
            let f = filter(d, q.table.name);
            q.filter = Some(q.filter.take().map_or(f.clone(), |old| old.and(f)));
        }
        for join in &mut q.joins {
            if let Some(d) = registry
                .models
                .iter()
                .find(|d| d.schema.name == join.table.name)
            {
                join.on = join.on.clone().and(filter(d, join.alias));
            }
        }
    }
    ast
}
fn eq(table: &'static str, col: &'static str, value: D) -> Expr {
    Expr::Compare {
        left: Box::new(Expr::Column(ColumnRef::new(table, col))),
        op: che_orm::CompareOp::Eq,
        right: Box::new(Expr::Value(value)),
    }
}
async fn has_draft(
    tx: &Transaction,
    d: &crate::model::Descriptor,
    id: Uuid,
) -> Result<bool, OrmError> {
    let base = shadow(tx, d, id).await?;
    let local = read(tx, d, id, false).await?;
    Ok(
        match crate::merge::patch(d, base.record.as_ref(), local.as_ref()) {
            Some(patch) => !patch.is_empty() || base.record.is_none(),
            None => base.record.is_some(),
        },
    )
}
pub(crate) async fn can_send(
    tx: &Transaction,
    model: &str,
    id: Uuid,
    deleting: bool,
) -> Result<bool, OrmError> {
    if meta::<u32>(tx, "model_log_protocol").await? != Some(OBJECT_LOG_PROTOCOL) {
        return Ok(true);
    }
    let rows=tx.query_sql("SELECT readable,changeable,deletable,status FROM _sync_object_access WHERE model=$1 AND entity_id=$2",vec![text(model),D::Uuid(id)]).await?;
    if let Some(r) = rows.first() {
        return Ok(r.get::<bool>(0)?
            && r.get::<bool>(if deleting { 2 } else { 1 })?
            && r.get::<String>(3)? == "active");
    }
    let rows = tx
        .query_sql(
            "SELECT can_create FROM _sync_object_models WHERE model=$1",
            vec![text(model)],
        )
        .await?;
    Ok(rows
        .first()
        .map(|r| r.get::<bool>(0))
        .transpose()?
        .unwrap_or(false))
}
pub(crate) async fn require_local_write(
    tx: &Transaction,
    model: &str,
    id: Uuid,
    deleting: bool,
) -> Result<(), OrmError> {
    if !can_send(tx, model, id, deleting).await? {
        return Err(invalid(
            "object access revoked or offline draft requires explicit review",
        ));
    }
    Ok(())
}

// The sent intent is known locally even when its canonical receipt is withheld.
// Keep it separate from the authoritative shadow until fresh state is delivered.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct AcceptedIntent {
    pub base_revision: i64,
    pub record: Option<crate::protocol::Record>,
}
pub(crate) fn accepted_intent_key(model: &str, id: Uuid) -> String {
    format!("object-accepted:{model}:{id}")
}

impl SyncClient {
    pub(crate) async fn observe_disclosures(
        &self,
        objects: Vec<(String, Uuid, bool)>,
    ) -> Result<(), OrmError> {
        let _guard = self.writes.lock().await;
        self.db.transaction(move|tx|{ Box::pin(async move {
            for (model,id,readable) in objects {
                if !readable {
                    tx.execute_sql("INSERT INTO _sync_object_access (model,entity_id,readable,changeable,deletable,status) VALUES ($1,$2,$3,$3,$3,'deleted') ON CONFLICT (model,entity_id) DO UPDATE SET readable=excluded.readable,changeable=excluded.changeable,deletable=excluded.deletable,status=excluded.status",vec![text(model),D::Uuid(id),D::Boolean(false)]).await?;
                    continue;
                }
                // Canonical content proves read access at response time. Write
                // rights are learned from the delivery page, never inferred.
                tx.execute_sql("INSERT INTO _sync_object_access (model,entity_id,readable,changeable,deletable,status) VALUES ($1,$2,$3,$4,$4,'active') ON CONFLICT (model,entity_id) DO UPDATE SET readable=excluded.readable",vec![text(model),D::Uuid(id),D::Boolean(true),D::Boolean(false)]).await?;
            }Ok(())
        }) }).await
    }
    pub(crate) async fn object_cursor(
        &self,
        model: &str,
    ) -> Result<Option<ObjectCursor>, OrmError> {
        let key = format!("object-cursor:{model}");
        self.db
            .transaction(move |tx| Box::pin(async move { meta(tx, &key).await }))
            .await
    }
    pub(crate) async fn apply_objects(&self, page: ObjectChanges) -> Result<bool, OrmError> {
        let _guard = self.writes.lock().await;
        let registry = self.registry.clone();
        self.db
            .transaction(move |tx| Box::pin(apply_page(tx, registry, page)))
            .await
    }
    pub(crate) async fn finish_without_state(
        &self,
        pending: Pending,
        outcome: WriteOutcome,
    ) -> Result<(), OrmError> {
        let _guard = self.writes.lock().await;
        let registry = self.registry.clone();
        self.db.transaction(move|tx|{ Box::pin(async move {
            let op=pending.request.operation.id;
            let row=tx.query_sql("SELECT payload FROM _sync_outbox WHERE operation_id=$1",vec![text(op)]).await?;
            let saved:Pending=decode(&row.first().ok_or_else(||invalid("missing pending operation"))?.get::<String>(0)?)?;
            if saved.request!=pending.request {return Err(invalid("settlement operation mismatch"));}
            for m in &pending.request.operation.mutations {
                if outcome == WriteOutcome::Accepted {
                    let d = registry.get(&m.model)?;
                    let base = shadow(tx, d, m.id).await?;
                    if base.revision != m.base_revision {
                        return Err(invalid("pending operation shadow base changed"));
                    }
                    let record = if let Some(changes) = &m.changes {
                        let mut sent = base.record.or(read(tx, d, m.id, false).await?).unwrap_or_default();
                        sent.extend(changes.clone());
                        Some(sent)
                    } else { None };
                    set_meta(tx, &accepted_intent_key(&m.model, m.id), &AcceptedIntent {
                        base_revision: m.base_revision, record,
                    }).await?;
                }
                tx.execute_sql("INSERT INTO _sync_object_access (model,entity_id,readable,changeable,deletable,status) VALUES ($1,$2,$3,$3,$3,'review') ON CONFLICT (model,entity_id) DO UPDATE SET readable=excluded.readable,changeable=excluded.changeable,deletable=excluded.deletable,status=excluded.status",vec![text(&m.model),D::Uuid(m.id),D::Boolean(false)]).await?;
            }
            // An opaque settlement can hide a stale receipt while read access
            // still exists. Replay the addressed feed to learn current rights
            // and state, even if its latest event was already downloaded.
            for m in &pending.request.operation.mutations {
                let key = format!("object-cursor:{}", m.model);
                if let Some(mut cursor) = meta::<ObjectCursor>(tx, &key).await? {
                    // Rewind delivery, retaining the authenticated feed/history.
                    cursor.sequence = 0;
                    set_meta(tx, &key, &cursor).await?;
                }
            }
            set_meta(tx,&format!("object-settled:{op}"),&outcome).await?;
            tx.execute_sql("DELETE FROM _sync_outbox WHERE operation_id=$1",vec![text(op)]).await?;
            Ok(())
        }) }).await
    }
    pub(crate) async fn object_blocked(&self) -> Result<bool, OrmError> {
        let registry = self.registry.clone();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    for r in tx
                        .query_sql("SELECT model,entity_id FROM _sync_dirty", vec![])
                        .await?
                    {
                        let model = r.get::<String>(0)?;
                        let id = r
                            .get::<String>(1)?
                            .parse()
                            .map_err(|_| invalid("invalid dirty UUID"))?;
                        let deleted = read(tx, registry.get(&model)?, id, false).await?.is_none();
                        if has_draft(tx, registry.get(&model)?, id).await?
                            && !can_send(tx, &model, id, deleted).await?
                        {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                })
            })
            .await
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ObjectStatus {
    pub model: String,
    pub id: Uuid,
    pub rights: ObjectRights,
    pub status: String,
    pub has_draft: bool,
    pub submitted: bool,
}
impl crate::ModelLogClient {
    /// Explicitly inspect access status. Retained drafts are never returned by
    /// ordinary model reads once access has been revoked.
    pub async fn object_status(&self) -> Result<Vec<ObjectStatus>, OrmError> {
        if !self.inner.object_mode {
            return Err(invalid("not an object client"));
        }
        let registry = self.inner.registry.clone();
        self.inner.db.transaction(move|tx|{ Box::pin(async move {
            let pending=tx.query_sql("SELECT payload FROM _sync_outbox",vec![]).await?.iter().map(|r|decode::<Pending>(&r.get::<String>(0)?)).collect::<Result<Vec<_>,_>>()?;
            let mut result=Vec::new();
            for r in tx.query_sql("SELECT model,entity_id,readable,changeable,deletable,status FROM _sync_object_access ORDER BY model,entity_id",vec![]).await? {
                let model=r.get::<String>(0)?;let id=r.get(1)?;
                let has_draft=has_draft(tx,registry.get(&model)?,id).await?;
                let submitted=pending.iter().any(|p|p.request.operation.mutations.iter().any(|m|m.model==model && m.id==id));
                result.push(ObjectStatus{model,id,rights:ObjectRights{read:r.get(2)?,change:r.get(3)?,delete:r.get(4)?},status:r.get(5)?,has_draft,submitted});
            } Ok(result)
        }) }).await
    }
    /// User-authorized recovery/export of a retained draft; it does not grant
    /// server access. Applications own their retention and export policy.
    pub async fn retained_draft(
        &self,
        model: &str,
        id: Uuid,
    ) -> Result<Option<crate::protocol::Record>, OrmError> {
        if !self.inner.object_mode {
            return Err(invalid("not an object client"));
        }
        let d = self.inner.registry.get(model)?.clone();
        self.inner
            .db
            .transaction(move |tx| Box::pin(async move { read(tx, &d, id, false).await }))
            .await
    }
    /// Explicitly allow a reviewed draft to send after access is restored.
    pub async fn resume_object(&self, model: &str, id: Uuid) -> Result<(), OrmError> {
        if !self.inner.object_mode {
            return Err(invalid("not an object client"));
        }
        let d = self.inner.registry.get(model)?.clone();
        let model = model.to_owned();
        let _guard = self.inner.writes.lock().await;
        self.inner.db.transaction(move |tx| Box::pin(async move {
            let deleting = deleting(tx, &model, id).await?
                && read(tx, &d, id, false).await?.is_none();
            let rows = tx.query_sql(
                "SELECT readable,changeable,deletable FROM _sync_object_access WHERE model=$1 AND entity_id=$2",
                vec![text(&model), D::Uuid(id)],
            ).await?;
            let allowed = rows.first().map(|r| {
                Ok::<_, OrmError>(r.get::<bool>(0)? && r.get::<bool>(if deleting { 2 } else { 1 })?)
            }).transpose()?.unwrap_or(false);
            if !allowed {
                return Err(invalid("current read and action rights required"));
            }
            tx.execute_sql(
                "UPDATE _sync_object_access SET status='active' WHERE model=$1 AND entity_id=$2",
                vec![text(model), D::Uuid(id)],
            ).await?;
            Ok(())
        })).await
    }
}

impl crate::ModelLogClient {
    /// Explicit retention decision, allowed only while the object is hidden and
    /// no submitted operation has an unknown outcome. No request is discarded.
    pub async fn discard_retained_draft(&self, model: &str, id: Uuid) -> Result<(), OrmError> {
        if !self.inner.object_mode {
            return Err(invalid("not an object client"));
        }
        let d = self.inner.registry.get(model)?.clone();
        let _guard = self.inner.writes.lock().await;
        self.inner.db.transaction(move|tx|{ Box::pin(async move {
            for row in tx.query_sql("SELECT payload FROM _sync_outbox",vec![]).await? {
                let p:Pending=decode(&row.get::<String>(0)?)?;
                if p.request.operation.mutations.iter().any(|m|m.model==d.name && m.id==id) {return Err(invalid("settle the submitted operation before discarding its draft"));}
            }
            let rows=tx.query_sql("SELECT readable FROM _sync_object_access WHERE model=$1 AND entity_id=$2",vec![text(d.name),D::Uuid(id)]).await?;
            if rows.first().map(|r|r.get::<bool>(0)).transpose()?.unwrap_or(true) {return Err(invalid("discard is only available for a hidden retained object"));}
            write(tx,&d,id,None,false).await?;
            write(tx,&d,id,None,true).await?;
            tx.execute_sql("DELETE FROM _sync_dirty WHERE model=$1 AND entity_id=$2",vec![text(d.name),text(id)]).await?;
            tx.execute_sql("DELETE FROM _sync_conflicts WHERE model=$1 AND entity_id=$2",vec![text(d.name),text(id)]).await?;
            set_deleting(tx,d.name,id,false).await?;
            tx.execute_sql("DELETE FROM _sync_meta WHERE key=$1",vec![text(accepted_intent_key(d.name,id))]).await?;
            tx.execute_sql("UPDATE _sync_object_access SET status='discarded' WHERE model=$1 AND entity_id=$2",vec![text(d.name),D::Uuid(id)]).await?;
            Ok(())
        }) }).await
    }
}

async fn apply_page(
    tx: &Transaction,
    registry: crate::Registry,
    page: ObjectChanges,
) -> Result<bool, OrmError> {
    let d = registry.get(&page.cursor.model)?;
    if page.protocol_version != OBJECT_LOG_PROTOCOL {
        return Err(invalid("incompatible object protocol"));
    }
    let key = format!("object-cursor:{}", d.name);
    let previous = meta::<ObjectCursor>(tx, &key).await?;
    let after = previous.as_ref().map_or(0, |c| c.sequence);
    if page.after != after
        || previous
            .as_ref()
            .is_some_and(|c| c.history != page.cursor.history || c.feed != page.cursor.feed)
    {
        return Err(invalid("object feed identity or boundary changed"));
    }
    let mut last = after;
    for e in &page.events {
        if Some(e.sequence) != last.checked_add(1) {
            return Err(invalid("object event sequence gap"));
        }
        last = e.sequence;
    }
    if last != page.cursor.sequence || (page.has_more && page.events.is_empty()) {
        return Err(invalid("object page cursor mismatch"));
    }
    tx.execute_sql("INSERT INTO _sync_object_models (model,can_create) VALUES ($1,$2) ON CONFLICT (model) DO UPDATE SET can_create=excluded.can_create",vec![text(d.name),D::Boolean(page.can_create)]).await?;
    let pending = tx
        .query_sql("SELECT payload FROM _sync_outbox", vec![])
        .await?
        .iter()
        .map(|r| decode::<Pending>(&r.get::<String>(0)?))
        .collect::<Result<Vec<_>, _>>()?;
    let deferred_key = format!("object-deferred:{}", d.name);
    let mut deferred = meta::<BTreeMap<Uuid, crate::protocol::Entity>>(tx, &deferred_key)
        .await?
        .unwrap_or_default();
    for e in &page.events {
        let (id, rights, status) = match &e.change {
            ObjectChange::State { state, rights } => {
                if state.model != d.name
                    || !rights.read
                    || state.values.is_none()
                    || state.revision <= 0
                {
                    return Err(invalid("invalid object state disclosure"));
                }
                registry.model_entity(state, Some(page.cursor.history))?;
                (state.record_id, *rights, "active")
            }
            ObjectChange::Revoked { record_id } => (*record_id, ObjectRights::default(), "revoked"),
            ObjectChange::Deleted { record_id, .. } => {
                (*record_id, ObjectRights::default(), "deleted")
            }
        };
        let dirty = has_draft(tx, d, id).await?;
        let old = tx
            .query_sql(
                "SELECT status FROM _sync_object_access WHERE model=$1 AND entity_id=$2",
                vec![text(d.name), D::Uuid(id)],
            )
            .await?;
        let status = if status == "active"
            && dirty
            && old
                .first()
                .is_some_and(|r| r.get::<String>(0).is_ok_and(|s| s != "active"))
        {
            "review"
        } else {
            status
        };
        tx.execute_sql("INSERT INTO _sync_object_access (model,entity_id,readable,changeable,deletable,status) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (model,entity_id) DO UPDATE SET readable=excluded.readable,changeable=excluded.changeable,deletable=excluded.deletable,status=excluded.status",vec![text(d.name),D::Uuid(id),D::Boolean(rights.read),D::Boolean(rights.change),D::Boolean(rights.delete),text(status)]).await?;
        match &e.change {
            ObjectChange::State { state, .. } => {
                deferred.insert(id, registry.model_entity(state, Some(page.cursor.history))?);
            }
            ObjectChange::Revoked { .. } | ObjectChange::Deleted { .. } => {
                deferred.remove(&id);
            }
        }
    }
    set_meta(tx, &deferred_key, &deferred).await?;
    drain_deferred(tx, &registry, d, &pending).await?;
    set_meta(tx, &key, &page.cursor).await?;
    // Existing merge/outbox code keeps its private model cursor anchor.
    set_meta(
        tx,
        &format!("cursor:model:{}", d.name),
        &Cursor {
            scope: crate::model_log::MODEL_LOG_SCOPE.into(),
            model: Some(d.name.into()),
            epoch: page.cursor.history,
            sequence: page.cursor.sequence,
        },
    )
    .await?;
    Ok(true)
}

// Keep each submitted request's shadow frozen without holding up the feed.
// Deferred disclosures are durable and superseded by later states/revocations.
async fn drain_deferred(
    tx: &Transaction,
    registry: &crate::Registry,
    d: &crate::model::Descriptor,
    pending: &[Pending],
) -> Result<(), OrmError> {
    let key = format!("object-deferred:{}", d.name);
    let mut deferred = meta::<BTreeMap<Uuid, crate::protocol::Entity>>(tx, &key)
        .await?
        .unwrap_or_default();
    let ids = deferred.keys().copied().collect::<Vec<_>>();
    for id in ids {
        if pending.iter().any(|p| {
            p.request
                .operation
                .mutations
                .iter()
                .any(|m| m.model == d.name && m.id == id)
        }) {
            continue;
        }
        let entity = deferred.remove(&id).expect("known deferred entity");
        let rows = tx
            .query_sql(
                "SELECT readable FROM _sync_object_access WHERE model=$1 AND entity_id=$2",
                vec![text(d.name), D::Uuid(id)],
            )
            .await?;
        if rows
            .first()
            .map(|r| r.get::<bool>(0))
            .transpose()?
            .unwrap_or(false)
        {
            let updates = crate::client::reconcile(tx, registry, vec![entity]).await?;
            crate::client::apply_updates(tx, registry, updates).await?;
            if !has_draft(tx, d, id).await? {
                tx.execute_sql(
                    "UPDATE _sync_object_access SET status='active' WHERE model=$1 AND entity_id=$2 AND status='review'",
                    vec![text(d.name), D::Uuid(id)],
                ).await?;
            }
        }
    }
    set_meta(tx, &key, &deferred).await
}

impl SyncClient {
    pub(crate) async fn apply_deferred_objects(&self) -> Result<(), OrmError> {
        let _guard = self.writes.lock().await;
        let registry = self.registry.clone();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    let pending = tx
                        .query_sql("SELECT payload FROM _sync_outbox", vec![])
                        .await?
                        .iter()
                        .map(|r| decode::<Pending>(&r.get::<String>(0)?))
                        .collect::<Result<Vec<_>, _>>()?;
                    for d in registry.models.iter() {
                        drain_deferred(tx, &registry, d, &pending).await?;
                    }
                    Ok(())
                })
            })
            .await
    }
}
