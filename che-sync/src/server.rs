use crate::{
    Registry, SyncModel, decode, encode, invalid,
    model::{Descriptor, database, record},
    protocol::*,
    storage::*,
};
use che_orm::{Database, DatabaseValue as D, IsolationLevel, OrmError, Transaction, Uuid};
use std::sync::Arc;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct PreparedEffect {
    pub model: String,
    pub entity_id: Uuid,
    pub handler: String,
    pub action: String,
    pub payload: String,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct PostCommitEffect {
    pub id: Uuid,
    pub prepared: PreparedEffect,
    pub previous: Option<Entity>,
    pub entity: Entity,
}

/// The application binds a server instance to a trusted principal and scope.
/// This policy runs for incoming writes, including implicit cascade effects.
/// Use OrmError::Configuration for permanent business rejections; operational
/// errors propagate without creating a receipt so the operation can be retried.
pub trait AccessPolicy: Send + Sync + 'static {
    fn isolation(&self) -> IsolationLevel {
        IsolationLevel::ReadCommitted
    }
    fn authorize_scope(&self, principal: &str, scope: &str) -> Result<(), OrmError>;
    /// Runs against the committing transaction, including no-ops and conflicts.
    fn authorize_write<'a>(
        &'a self,
        _tx: &'a Transaction,
        _current: &'a Entity,
        _mutation: &'a Mutation,
    ) -> che_orm::TransactionFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    /// Transactional application initialization, e.g. explicit creator grants.
    fn after_write<'a>(
        &'a self,
        _tx: &'a Transaction,
        _current: &'a Entity,
        _next: &'a Entity,
    ) -> che_orm::TransactionFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
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
    pub(crate) db: Database,
    orm_db: Database,
    pub(crate) registry: Registry,
    pub(crate) scope: String,
    pub(crate) principal: String,
    pub(crate) policy: Arc<dyn AccessPolicy>,
    pub(crate) model: Option<String>,
    pub(crate) model_logs: bool,
}

impl SyncServer {
    pub async fn from_schema(
        db: Database,
        schema: &che_orm::SchemaSet,
        scope: impl Into<String>,
        principal: impl Into<String>,
        policy: Arc<dyn AccessPolicy>,
    ) -> Result<Self, OrmError> {
        Self::open(db, Registry::from_schema(schema)?, scope, principal, policy).await
    }
    /// Tables must already be migrated. The principal is trusted transport
    /// context, never a field accepted from a PushRequest.
    pub async fn open(
        db: Database,
        registry: Registry,
        scope: impl Into<String>,
        principal: impl Into<String>,
        policy: Arc<dyn AccessPolicy>,
    ) -> Result<Self, OrmError> {
        registry.require_role(che_orm::ModelRole::Server)?;
        let scope = scope.into();
        let principal = principal.into();
        if scope.is_empty() || principal.is_empty() {
            return Err(invalid("empty scope or principal"));
        }
        policy.authorize_scope(&principal, &scope)?;
        db.execute_sql("INSERT INTO _sync_scopes (scope, epoch, sequence) VALUES ($1,$2,0) ON CONFLICT (scope) DO NOTHING", vec![text(&scope), text(Uuid::now_v7())]).await?;
        let mut server = Self {
            orm_db: db.clone(),
            db,
            registry,
            scope,
            principal,
            policy,
            model: None,
            model_logs: false,
        };
        server.orm_db = server
            .db
            .clone()
            .with_model_runtime(Arc::new(server.clone()));
        Ok(server)
    }

    /// Whole-model journals, without a user-supplied scope/workspace.
    /// Apply compiled journal migrations before opening the runtime.
    pub async fn open_model_logs(
        db: Database,
        registry: Registry,
        principal: impl Into<String>,
        policy: Arc<dyn AccessPolicy>,
    ) -> Result<Self, OrmError> {
        let mut server = Self::open(
            db,
            registry,
            crate::model_log::MODEL_LOG_SCOPE,
            principal,
            policy,
        )
        .await?;
        server.model_logs = true;
        let registry = server.registry.clone();
        server
            .db
            .transaction(move |tx| {
                Box::pin(async move { crate::model_log::lock_logs(tx, &registry).await })
            })
            .await?;
        server.orm_db = server
            .db
            .clone()
            .with_model_runtime(Arc::new(server.clone()));
        Ok(server)
    }

    pub async fn changes<M: SyncModel>(
        &self,
        after: i64,
        limit: u32,
    ) -> Result<crate::model_log::ModelChanges, OrmError> {
        use crate::model_log::{ChangeKind, ModelChanges, ModelOperation};
        if !self.model_logs || after < 0 || !(1..=1000).contains(&limit) {
            return Err(invalid("invalid model journal request"));
        }
        self.policy.authorize_scope(&self.principal, &self.scope)?;
        let d = self.registry.typed::<M>()?.clone();
        self.db.transaction_with_isolation(IsolationLevel::RepeatableRead, move |tx| Box::pin(async move {
            let state = tx.query_sql(format!("SELECT sequence, history FROM {} WHERE key = 0", quote(&d.log_state_table())), vec![]).await?;
             let state = state.first().ok_or_else(|| invalid("uninitialized model log"))?;
             if state.get::<i64>(0)? == 0 && !tx.query_sql(format!("SELECT {} FROM {} LIMIT 1", quote(d.primary), quote(d.schema.name)), vec![]).await?.is_empty() {
                 return Err(invalid("existing records require explicit model journal backfill"));
             }
            if after > state.get::<i64>(0)? { return Err(invalid("model cursor is ahead of server history")); }
            let rows = tx.query_sql(format!("SELECT id, record_id, revision, kind, diff, created_at, schema_version FROM {} WHERE id > $1 ORDER BY id LIMIT $2", quote(&d.log_table())), vec![D::Integer(after), D::Integer(i64::from(limit) + 1)]).await?;
            let has_more = rows.len() > limit as usize;
            let mut operations = Vec::new();
            for row in rows.into_iter().take(limit as usize) {
                let kind = match row.get::<String>(3)?.as_str() { "create" => ChangeKind::Create, "update" => ChangeKind::Update, "delete" => ChangeKind::Delete, _ => return Err(invalid("invalid journal operation")) };
                operations.push(ModelOperation { id: row.get(0)?, record_id: row.get(1)?, revision: row.get(2)?, kind, diff: decode(&row.get::<String>(4)?)?, created_at: row.get(5)?, schema_version: u32::try_from(row.get::<i64>(6)?).map_err(|_| invalid("invalid schema version"))? });
            }
            Ok(ModelChanges { model: d.name.into(), history: state.get::<String>(1)?.parse().map_err(|_| invalid("invalid model history"))?, next_cursor: operations.last().map_or(after, |operation| operation.id), operations, has_more })
        })).await
    }

    /// Explicit baseline for an existing, already migrated table. Never called
    /// by server startup or ordinary GET requests. Existing revisions survive.
    pub async fn backfill_model_log<M: SyncModel>(&self) -> Result<usize, OrmError> {
        if !self.model_logs {
            return Err(invalid("model journals are not enabled"));
        }
        self.policy.authorize_scope(&self.principal, &self.scope)?;
        let server = self.clone();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    crate::model_log::lock_logs(tx, &server.registry).await?;
                    let d = server.registry.typed::<M>()?;
                    if !tx
                        .query_sql(
                            format!("SELECT id FROM {} LIMIT 1", quote(&d.log_table())),
                            vec![],
                        )
                        .await?
                        .is_empty()
                    {
                        return Err(invalid("model journal already has operations"));
                    }
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
                    for row in &rows {
                        let id = row.get::<Uuid>(0)?;
                        let raw = tx
                            .query_sql(
                                format!(
                                    "SELECT {}, {} FROM {} WHERE {} = $1",
                                    quote(d.revision),
                                    quote(d.scope),
                                    quote(d.schema.name),
                                    quote(d.primary)
                                ),
                                vec![D::Uuid(id)],
                            )
                            .await?;
                        let revision = raw[0].get::<i64>(0)?;
                        let scope = raw[0].get::<String>(1)?;
                        if !scope.is_empty() && scope != server.scope {
                            return Err(invalid(
                                "baseline requires an explicit legacy scope migration",
                            ));
                        }
                        stamp(tx, d, id, revision.max(1), &server.scope).await?;
                        let next = server.entity(tx, d, id).await?;
                        let mut absent = next.clone();
                        absent.record = None;
                        absent.revision = 0;
                        // Retained tombstones need an explicit deletion even if their
                        // former business record is no longer available to a client.
                        crate::model_log::append(tx, &server.registry, &absent, &next).await?;
                    }
                    Ok(rows.len())
                })
            })
            .await
    }

    /// Scope-bound ORM handle. Writes journal changes in the same transaction.
    pub fn database(&self) -> &Database {
        &self.orm_db
    }

    /// Bind a feed to one model. FK parents are included automatically; the
    /// full registry is retained to authorize and journal cascading effects.
    pub fn for_model<M: SyncModel>(&self) -> Result<Self, OrmError> {
        self.registry.check_model::<M>()?;
        let mut server = self.clone();
        server.model = Some(M::SYNC_NAME.into());
        Ok(server)
    }
    pub fn for_model_name(&self, name: &str) -> Result<Self, OrmError> {
        self.registry.get(name)?;
        let mut server = self.clone();
        server.model = Some(name.into());
        Ok(server)
    }

    pub async fn push(&self, request: PushRequest) -> Result<PushResult, OrmError> {
        self.push_internal(request, false).await
    }

    async fn push_internal(
        &self,
        request: PushRequest,
        trusted: bool,
    ) -> Result<PushResult, OrmError> {
        self.push_transaction(request, trusted, None, vec![], None, None)
            .await
            .map(|(result, _)| result)
    }

    /// Capture the touched rows and cascade dependents for pre-commit lifecycle
    /// preparation. push_prepared checks these revisions under the scope lock.
    pub async fn prepare_push(&self, request: &PushRequest) -> Result<Vec<Entity>, OrmError> {
        self.policy.authorize_scope(&self.principal, &self.scope)?;
        if request.scope != self.scope {
            return Err(invalid("invalid request scope"));
        }
        let server = self.clone();
        let mutations = request.operation.mutations.clone();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    let mut entities = Vec::new();
                    for mutation in &mutations {
                        entities.push(
                            server
                                .entity(tx, server.registry.get(&mutation.model)?, mutation.id)
                                .await?,
                        );
                    }
                    for mutation in mutations.iter().filter(|m| m.changes.is_none()) {
                        collect_dependents(
                            tx,
                            &server,
                            &mutation.model,
                            mutation.id,
                            &mut entities,
                        )
                        .await?;
                    }
                    Ok(entities)
                })
            })
            .await
    }

    /// Returns whether this operation committed now rather than replaying a
    /// receipt. A changed preparation returns a conflict without committing.
    pub async fn push_prepared(
        &self,
        request: PushRequest,
        before: Vec<Entity>,
    ) -> Result<(PushResult, bool), OrmError> {
        self.push_transaction(request, false, Some(before), vec![], None, None)
            .await
    }

    pub async fn push_with_effects(
        &self,
        request: PushRequest,
        before: Vec<Entity>,
        effects: Vec<PreparedEffect>,
    ) -> Result<(PushResult, bool), OrmError> {
        self.push_transaction(request, false, Some(before), effects, None, None)
            .await
    }
    pub async fn pending_effects(&self) -> Result<Vec<PostCommitEffect>, OrmError> {
        self.policy.authorize_scope(&self.principal, &self.scope)?;
        self.db
            .query_sql(
                "SELECT payload FROM _sync_effects WHERE scope = $1 AND principal = $2 ORDER BY id",
                vec![text(&self.scope), text(&self.principal)],
            )
            .await?
            .iter()
            .map(|row| decode(&row.get::<String>(0)?))
            .collect()
    }

    /// Replay request receipts before validation or loading a deleted row.
    pub async fn request_receipt(
        &self,
        replica: Uuid,
        operation: Uuid,
        identity: &str,
    ) -> Result<Option<PushResult>, OrmError> {
        self.policy.authorize_scope(&self.principal, &self.scope)?;
        let replica = format!("{}:{}{}", self.principal.len(), self.principal, replica);
        let rows = self.db.query_sql("SELECT request, result FROM _sync_receipts WHERE scope = $1 AND replica_id = $2 AND operation_id = $3", vec![text(&self.scope), text(replica), text(operation)]).await?;
        match rows.first() {
            None => Ok(None),
            Some(row) => {
                if decode::<(String, PushRequest)>(&row.get::<String>(0)?)?.0 != identity {
                    return Err(invalid("operation ID reused with different contents"));
                }
                Ok(Some(decode(&row.get::<String>(1)?)?))
            }
        }
    }

    pub async fn write_prepared_request(
        &self,
        request: PushRequest,
        before: Vec<Entity>,
        effects: Vec<PreparedEffect>,
        identity: String,
    ) -> Result<PushResult, OrmError> {
        self.push_transaction(request, true, Some(before), effects, Some(identity), None)
            .await
            .map(|(result, _)| result)
    }
    /// Final serializer/domain rejection. Shares receipt locking and replay with
    /// accepted writes, so an already committed identical request always wins.
    pub async fn reject_request(
        &self,
        request: PushRequest,
        identity: String,
        reason: String,
    ) -> Result<PushResult, OrmError> {
        self.push_transaction(request, true, None, vec![], Some(identity), Some(reason))
            .await
            .map(|(result, _)| result)
    }
    pub async fn acknowledge_effect(&self, id: Uuid) -> Result<(), OrmError> {
        self.db
            .execute_sql(
                "DELETE FROM _sync_effects WHERE id = $1 AND scope = $2 AND principal = $3",
                vec![text(id), text(&self.scope), text(&self.principal)],
            )
            .await?;
        Ok(())
    }

    async fn push_transaction(
        &self,
        request: PushRequest,
        trusted: bool,
        prepared: Option<Vec<Entity>>,
        effects: Vec<PreparedEffect>,
        request_identity: Option<String>,
        rejection: Option<String>,
    ) -> Result<(PushResult, bool), OrmError> {
        self.policy.authorize_scope(&self.principal, &self.scope)?;
        if request.protocol_version != PROTOCOL_VERSION || request.scope != self.scope {
            return Err(invalid("incompatible push protocol or scope"));
        }
        if request.operation.mutations.is_empty() {
            return Err(invalid("empty operation"));
        }
        if let Some(model) = &self.model
            && request
                .operation
                .mutations
                .iter()
                .any(|m| &m.model != model)
        {
            return Err(invalid("operation contains a model outside this feed"));
        }
        let server = self.clone();
        // READ COMMITTED plus a scope row lock yields commit-ordered groups in
        // PostgreSQL. SQLite's write lock provides the same serialization.
        self.db.transaction_with_isolation(self.policy.isolation(), move |tx| Box::pin(async move {
             tx.execute_sql("UPDATE _sync_scopes SET sequence = sequence WHERE scope = $1", vec![text(&server.scope)]).await?;
             if server.model_logs { crate::model_log::lock_logs(tx, &server.registry).await?; }
            let request_json = match &request_identity { Some(identity) => encode(&(identity, &request))?, None => encode(&request)? };
            let receipt_replica = format!("{}:{}", server.principal.len(), server.principal) + &request.replica_id.to_string();
            let rows = tx.query_sql("SELECT request, result FROM _sync_receipts WHERE scope = $1 AND replica_id = $2 AND operation_id = $3", vec![text(&server.scope), text(&receipt_replica), text(request.operation.id)]).await?;
            if let Some(row) = rows.first() {
                let same = match &request_identity {
                    Some(identity) => decode::<(String, PushRequest)>(&row.get::<String>(0)?)?.0 == *identity,
                    None => decode::<PushRequest>(&row.get::<String>(0)?)? == request,
                };
                if !same { return Err(invalid("operation ID reused with different contents")); }
                 return Ok((decode(&row.get::<String>(1)?)?, false));
             }
             if server.model_logs { crate::model_log::require_baselines(tx, &server.registry).await?; }
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
                    server.policy.authorize_write(tx, &entity, m).await?;
                    if entity.revision == 0 && m.changes.is_none() { return Err(invalid("cannot delete an unknown UUID")); }
                    if entity.revision != m.base_revision { conflict = true; }
                    if entity.revision == m.base_revision && entity.revision > 0 && entity.record.is_none() && m.changes.is_some() { return Err(invalid("deleted UUID cannot be resurrected")); }
                    current.push(entity);
                }
                Ok::<_, OrmError>(())
            }.await;
             let result = if let Some(reason) = rejection {
                 PushResult::Rejected { reason }
             } else if let Err(error) = checked {
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
                if prepared.as_ref().is_some_and(|prepared| prepared.len() != before.len() || before.iter().any(|entity| !prepared.contains(entity))) {
                    return Ok((PushResult::Conflict(current), false));
                }
                savepoint(tx).await?;
                let application = server.apply_and_journal(tx, &request.operation.mutations, &before).await;
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
            let committed = matches!(result, PushResult::Accepted(_));
            if let PushResult::Accepted(entities) = &result {
                for effect_preparation in effects {
                     if let Some(entity) = entities.iter().find(|entity| entity.model == effect_preparation.model && entity.id == effect_preparation.entity_id) {
                         if server.model_logs && prepared.as_ref().is_some_and(|before| before.iter().any(|old| old.model == entity.model && old.id == entity.id && old.revision == entity.revision)) { continue; }
                        let previous = prepared.as_ref().and_then(|before| before.iter().find(|old| old.model == entity.model && old.id == entity.id)).cloned();
                        let effect = PostCommitEffect { id: Uuid::now_v7(), previous, entity: entity.clone(), prepared: effect_preparation };
                        tx.execute_sql("INSERT INTO _sync_effects (id, scope, principal, payload) VALUES ($1,$2,$3,$4)", vec![text(effect.id), text(&server.scope), text(&server.principal), text(encode(&effect)?)]).await?;
                    }
                }
            }
            Ok((result, committed))
        })).await
    }

    pub async fn pull(&self, request: PullRequest) -> Result<PullResult, OrmError> {
        self.policy.authorize_scope(&self.principal, &self.scope)?;
        if request.protocol_version != PROTOCOL_VERSION
            || request.limit == 0
            || request.limit > 1000
        {
            return Err(invalid("invalid pull request"));
        }
        let Some(cursor) = request.cursor else {
            return self.snapshot().await.map(PullResult::Full);
        };
        if request.protocol_version != PROTOCOL_VERSION
            || cursor.scope != self.scope
            || cursor.model != self.model
            || cursor.sequence < 0
            || request.limit == 0
            || request.limit > 1000
        {
            return Err(invalid("invalid pull request"));
        }
        let server = self.clone();
        self.db.transaction_with_isolation(IsolationLevel::RepeatableRead, move |tx| Box::pin(async move {
            let current = server.cursor(tx).await?;
            if current.epoch != cursor.epoch || cursor.sequence > current.sequence {
                return Err(invalid("server database replacement or history rewind is not supported"));
            }
            let rows = tx.query_sql("SELECT payload FROM _sync_changes WHERE scope = $1 AND sequence > $2 ORDER BY sequence LIMIT $3", vec![text(&server.scope), D::Integer(cursor.sequence), D::Integer(i64::from(request.limit))]).await?;
            let mut groups: Vec<ChangeGroup> = rows.iter().map(|row| decode(&row.get::<String>(0)?)).collect::<Result<_, _>>()?;
            let sequence = groups.last().map_or(cursor.sequence, |g| g.sequence);
            // A missing interval means retained history is no longer sufficient.
            if groups.iter().enumerate().any(|(index, group)| group.sequence != cursor.sequence + index as i64 + 1) || (groups.is_empty() && sequence < current.sequence) { return Ok(PullResult::ResetRequired); }
            if let Some(model) = &server.model {
                let allowed = server.registry.feed_models(model)?;
                let related = server.registry.related_models(model)?;
                for group in &mut groups {
                    let deleting = group.entities.iter().any(|e| allowed.contains(&e.model) && e.record.is_none());
                    group.entities.retain(|e| allowed.contains(&e.model) || (deleting && related.contains(&e.model)));
                }
            }
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
    /// Apply a serializer-validated create or single-primary-key update through
    /// the journal. Prepared server-owned values are trusted; managed timestamps
    /// are still supplied by the server. Arbitrary update predicates are rejected.
    pub async fn write<M: SyncModel>(
        &self,
        write: che_orm::ValidatedWrite<M>,
    ) -> Result<PushResult, OrmError> {
        let request = self.prepare_write(write).await?;
        self.push_internal(request, true).await
    }

    /// Convert trusted serializer output to a mutation without committing it.
    pub async fn prepare_write<M: SyncModel>(
        &self,
        write: che_orm::ValidatedWrite<M>,
    ) -> Result<PushRequest, OrmError> {
        self.prepare_write_at(write, None).await
    }

    /// Encode a conditional mutation against the caller's captured revision. Existence and
    /// revision checks run after receipt replay under the transaction scope lock,
    /// so concurrent retries cannot fail before resolving their receipt.
    pub async fn prepare_conditional_write<M: SyncModel>(
        &self,
        write: che_orm::ValidatedWrite<M>,
        revision: i64,
    ) -> Result<PushRequest, OrmError> {
        if revision < 0 {
            return Err(invalid("negative base revision"));
        }
        self.prepare_write_at(write, Some(revision)).await
    }

    async fn prepare_write_at<M: SyncModel>(
        &self,
        write: che_orm::ValidatedWrite<M>,
        revision: Option<i64>,
    ) -> Result<PushRequest, OrmError> {
        use che_orm::{CompareOp, Expr, QueryAst, ValidatedWrite};
        let d = self.registry.typed::<M>()?;
        let (id, values, creating) = match write {
            ValidatedWrite::Create(query) => {
                let ast = query.into_ast()?;
                ast.validate()?;
                let QueryAst::Insert(ast) = ast else {
                    unreachable!()
                };
                let id = ast
                    .values
                    .iter()
                    .find_map(|v| {
                        if v.column.name == d.primary
                            && let D::Uuid(id) = v.value
                        {
                            return Some(id);
                        }
                        None
                    })
                    .ok_or_else(|| invalid("missing UUID in validated create"))?;
                (
                    id,
                    ast.values
                        .into_iter()
                        .map(|v| (v.column.name, v.value))
                        .collect::<Vec<_>>(),
                    true,
                )
            }
            ValidatedWrite::Update(query) => {
                let ast = query.into_ast()?;
                ast.validate()?;
                let QueryAst::Update(ast) = ast else {
                    unreachable!()
                };
                let Some(Expr::Compare {
                    left,
                    op: CompareOp::Eq,
                    right,
                }) = ast.filter
                else {
                    return Err(invalid(
                        "sync write requires a single primary-key predicate",
                    ));
                };
                let (Expr::Column(column), Expr::Value(D::Uuid(id))) = (*left, *right) else {
                    return Err(invalid("sync write requires a UUID predicate"));
                };
                if column.table != d.schema.name || column.name != d.primary {
                    return Err(invalid("sync write requires a primary-key predicate"));
                }
                (
                    id,
                    ast.assignments
                        .into_iter()
                        .map(|v| (v.column.name, v.value))
                        .collect::<Vec<_>>(),
                    false,
                )
            }
        };
        let mut changes = Record::new();
        for (name, value) in values {
            let column = d
                .schema
                .columns
                .iter()
                .find(|c| c.name == name)
                .ok_or_else(|| invalid("unknown write column"))?;
            if column.primary_key
                || column.auto_now
                || column.auto_now_add
                || [d.revision, d.updated_at, d.deleted_at, d.scope].contains(&column.name)
            {
                continue;
            }
            let field = d.fields.iter().find(|f| f.column == name).unwrap();
            changes.insert(field.name.into(), crate::model::from_database(&value));
        }
        let base_revision = if let Some(revision) = revision {
            revision
        } else {
            let descriptor = d.clone();
            let server = self.clone();
            let old = self
                .db
                .transaction(move |tx| {
                    Box::pin(async move { server.entity(tx, &descriptor, id).await })
                })
                .await?;
            if creating && old.revision != 0 {
                return Err(invalid("UUID already exists"));
            }
            if !creating && old.record.is_none() {
                return Err(invalid("cannot update an absent entity"));
            }
            old.revision
        };
        Ok(PushRequest {
            protocol_version: PROTOCOL_VERSION,
            scope: self.scope.clone(),
            replica_id: Uuid::nil(),
            operation: Operation {
                id: Uuid::now_v7(),
                mutations: vec![Mutation {
                    model: M::SYNC_NAME.into(),
                    id,
                    schema_version: M::SYNC_VERSION,
                    base_revision,
                    changes: Some(changes),
                }],
            },
        })
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
                !column.primary_key
                    && !column.auto_now
                    && !column.auto_now_add
                    && ![d.revision, d.updated_at, d.deleted_at, d.scope].contains(&column.name)
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
            model: self.model.clone(),
            epoch: row
                .get::<String>(0)?
                .parse()
                .map_err(|_| invalid("invalid epoch"))?,
            sequence: row.get(1)?,
        })
    }

    pub(crate) async fn entity(
        &self,
        tx: &Transaction,
        d: &Descriptor,
        id: Uuid,
    ) -> Result<Entity, OrmError> {
        let rows = tx
            .query_sql(
                format!(
                    "SELECT {}, {}, {}, {} FROM {} WHERE {} = $1",
                    quote(d.scope),
                    quote(d.revision),
                    quote(d.updated_at),
                    quote(d.deleted_at),
                    quote(d.schema.name),
                    quote(d.primary)
                ),
                vec![D::Uuid(id)],
            )
            .await?;
        let (revision, updated_at, deleted_at) = if let Some(row) = rows.first() {
            if row.get::<String>(0)? != self.scope {
                return Err(invalid("entity belongs to another scope"));
            }
            (
                row.get::<i64>(1)?,
                row.get::<Option<che_orm::time::OffsetDateTime>>(2)?,
                row.get::<Option<che_orm::time::OffsetDateTime>>(3)?,
            )
        } else if self.model_logs {
            // SQL cascades from ordinary FK parents may physically remove a row.
            // Its journal retains identity/revision so a stale create cannot resurrect it.
            let rows = tx.query_sql(
                format!("SELECT revision, created_at, kind FROM {} WHERE record_id = $1 ORDER BY id DESC LIMIT 1", quote(&d.log_table())),
                vec![D::Uuid(id)],
            ).await?;
            if let Some(row) = rows.first() {
                let timestamp = row.get::<che_orm::time::OffsetDateTime>(1)?;
                (
                    row.get::<i64>(0)?,
                    Some(timestamp),
                    (row.get::<String>(2)? == "delete").then_some(timestamp),
                )
            } else {
                (0, None, None)
            }
        } else {
            (0, None, None)
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
            epoch: Some(self.cursor(tx).await?.epoch),
            updated_at,
            deleted_at,
            record,
        })
    }

    async fn inventory(&self, tx: &Transaction) -> Result<Vec<Entity>, OrmError> {
        let mut entities = Vec::new();
        let mut allowed = self
            .model
            .as_deref()
            .map(|model| self.registry.feed_models(model))
            .transpose()?;
        // A reset must carry current cascade effects as well, including SET
        // NULL rows whose former parent can no longer be inferred from the row.
        if let (Some(model), Some(feed)) = (&self.model, &allowed) {
            let mut deleted = false;
            for d in self
                .registry
                .models
                .iter()
                .filter(|d| feed.contains(d.name))
            {
                deleted |= !tx
                    .query_sql(
                        format!(
                            "SELECT {} FROM {} WHERE {} = $1 AND {} IS NOT NULL LIMIT 1",
                            quote(d.primary),
                            quote(d.schema.name),
                            quote(d.scope),
                            quote(d.deleted_at)
                        ),
                        vec![text(&self.scope)],
                    )
                    .await?
                    .is_empty();
            }
            if deleted {
                allowed = Some(self.registry.related_models(model)?);
            }
        }
        for d in self.registry.models.iter() {
            let model = d.name.to_owned();
            if allowed
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(&model))
            {
                continue;
            }
            let rows = tx
                .query_sql(
                    format!(
                        "SELECT {} FROM {} WHERE {} = $1 ORDER BY {}",
                        quote(d.primary),
                        quote(d.schema.name),
                        quote(d.scope),
                        quote(d.primary)
                    ),
                    vec![text(&self.scope)],
                )
                .await?;
            for row in rows {
                entities.push(self.entity(tx, d, row.get::<Uuid>(0)?).await?);
            }
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
                    format!(
                        "SELECT {}, {} FROM {} WHERE {} = $1",
                        quote(parent.scope),
                        quote(parent.deleted_at),
                        quote(parent.schema.name),
                        quote(parent.primary)
                    ),
                    vec![D::Uuid(*id)],
                )
                .await?;
            if let Some(row) = rows.first() {
                if row.get::<String>(0)? != self.scope {
                    return Err(invalid("cross-scope foreign key"));
                }
                if row
                    .get::<Option<che_orm::time::OffsetDateTime>>(1)?
                    .is_some()
                {
                    return Err(invalid("foreign key target is deleted"));
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

impl che_orm::ModelRuntime for SyncServer {
    fn captures_table(&self, table: &str) -> bool {
        self.role(table).is_some()
            || (self.model_logs
                && self.registry.models.iter().any(|model| {
                    model.schema.columns.iter().any(|column| {
                        column.references.is_some()
                            && column.column_type == che_orm::ColumnType::Integer
                    })
                }))
    }
    fn role(&self, table: &str) -> Option<che_orm::ModelRole> {
        self.registry
            .models
            .iter()
            .any(|d| d.schema.name == table)
            .then_some(che_orm::ModelRole::Server)
    }

    fn prepare_read(&self, ast: che_orm::QueryAst) -> che_orm::QueryAst {
        crate::runtime::prepare_read(&self.registry, ast, Some(&self.scope))
    }

    fn authorize_read(&self, ast: &che_orm::QueryAst) -> Result<(), OrmError> {
        if let che_orm::QueryAst::Select(select) = ast
            && std::iter::once(select.table.name)
                .chain(select.joins.iter().map(|join| join.table.name))
                .any(|table| self.registry.models.iter().any(|d| d.schema.name == table))
        {
            self.policy.authorize_scope(&self.principal, &self.scope)?;
        }
        Ok(())
    }

    fn write<'a>(
        &'a self,
        tx: &'a Transaction,
        ast: che_orm::QueryAst,
    ) -> che_orm::TransactionFuture<'a, che_orm::ModelWriteResult> {
        Box::pin(async move {
            self.policy.authorize_scope(&self.principal, &self.scope)?;
            // Lock before selecting targets so journal sequences follow commits.
            tx.execute_sql(
                "UPDATE _sync_scopes SET sequence = sequence WHERE scope = $1",
                vec![text(&self.scope)],
            )
            .await?;
            if self.model_logs {
                crate::model_log::lock_logs(tx, &self.registry).await?;
                crate::model_log::require_baselines(tx, &self.registry).await?;
            }
            savepoint(tx).await?;
            let write = self.orm_write(tx, &ast).await;
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

impl SyncServer {
    async fn orm_write(
        &self,
        tx: &Transaction,
        ast: &che_orm::QueryAst,
    ) -> Result<che_orm::ModelWriteResult, OrmError> {
        use che_orm::QueryAst;
        let table = match ast {
            QueryAst::Insert(ast) => ast.table.name,
            QueryAst::Update(ast) => ast.table.name,
            QueryAst::Delete(ast) => ast.table.name,
            _ => return Err(invalid("expected mutation")),
        };
        if !self
            .registry
            .models
            .iter()
            .any(|model| model.schema.name == table)
        {
            return self.external_write(tx, ast).await;
        }
        let d = crate::runtime::descriptor(&self.registry, ast)?;
        let ids = crate::runtime::targets(tx, d, ast, Some(&self.scope)).await?;
        if ids.is_empty() {
            return crate::runtime::result(tx, d, ast, &ids).await;
        }
        let values = match ast {
            QueryAst::Insert(ast) => ast
                .values
                .iter()
                .map(|v| (v.column.name, &v.value))
                .collect::<Vec<_>>(),
            QueryAst::Update(ast) => ast
                .assignments
                .iter()
                .map(|v| (v.column.name, &v.value))
                .collect(),
            _ => vec![],
        };
        let mut changes = Record::new();
        for (name, value) in values {
            let column = d
                .schema
                .columns
                .iter()
                .find(|column| column.name == name)
                .ok_or_else(|| invalid("unknown write column"))?;
            if column.primary_key {
                if matches!(ast, QueryAst::Update(_)) {
                    return Err(invalid("primary key is immutable"));
                }
                continue;
            }
            if column.auto_now
                || column.auto_now_add
                || [d.revision, d.updated_at, d.deleted_at, d.scope].contains(&name)
            {
                continue;
            }
            let field = d.fields.iter().find(|field| field.column == name).unwrap();
            changes.insert(field.name.into(), crate::model::from_database(value));
        }
        let mut before = Vec::new();
        let mut mutations = Vec::new();
        for id in &ids {
            let old = self.entity(tx, d, *id).await?;
            if matches!(ast, QueryAst::Insert(_)) && old.revision != 0 {
                return Err(invalid("UUID already exists"));
            }
            let changes = if matches!(ast, QueryAst::Delete(_)) {
                None
            } else {
                Some(changes.clone())
            };
            if let Some(changes) = &changes {
                d.validate(changes, *id, false, false)?;
            }
            let mutation = Mutation {
                model: d.name.into(),
                id: *id,
                schema_version: d.version,
                base_revision: old.revision,
                changes,
            };
            self.policy.authorize_write(tx, &old, &mutation).await?;
            mutations.push(mutation);
            before.push(old);
        }
        for mutation in mutations.iter().filter(|m| m.changes.is_none()) {
            collect_dependents(tx, self, d.name, mutation.id, &mut before).await?;
        }
        self.apply_and_journal(tx, &mutations, &before).await?;
        crate::runtime::result(tx, d, ast, &ids).await
    }

    /// Shared transactional application for prepared requests and ORM writes.
    /// Callers hold the scope lock and own a rollback savepoint.
    async fn apply_and_journal(
        &self,
        tx: &Transaction,
        mutations: &[Mutation],
        before: &[Entity],
    ) -> Result<Vec<Entity>, OrmError> {
        let mut plan = Vec::new();
        for d in self.registry.models.iter() {
            for mutation in mutations
                .iter()
                .filter(|m| m.model == d.name && m.changes.is_some())
            {
                self.check_references(tx, d, mutation.changes.as_ref().unwrap(), mutations)
                    .await?;
                plan.push(mutation.clone());
            }
        }
        for d in self.registry.models.iter().rev() {
            plan.extend(
                mutations
                    .iter()
                    .filter(|m| m.model == d.name && m.changes.is_none())
                    .cloned(),
            );
        }
        let plan = Arc::new(plan);
        let writer = self.clone();
        let owned_mutations = Arc::new(mutations.to_vec());
        ordered(tx, plan.len(), move |tx, index| {
            let plan = plan.clone();
            let server = writer.clone();
            let mutations = owned_mutations.clone();
            Box::pin(async move {
                let mutation = &plan[index];
                let d = server.registry.get(&mutation.model)?;
                if let Some(changes) = &mutation.changes {
                    if server.model_logs
                        && let Some(current) = read(tx, d, mutation.id, false).await?
                        && changes
                            .iter()
                            .all(|(field, value)| current.get(field) == Some(value))
                    {
                        return Ok(());
                    }
                    apply_patch(tx, d, mutation).await
                } else {
                    guard_moves(tx, &server.registry, d, mutation.id, &mutations).await?;
                    soft_delete(tx, &server.registry, d, mutation.id).await
                }
            })
        })
        .await?;
        let mut affected = Vec::new();
        for old in before {
            let descriptor = self.registry.get(&old.model)?;
            let next_record = read(tx, descriptor, old.id, false).await?;
            if next_record != old.record
                || (!self.model_logs
                    && mutations
                        .iter()
                        .any(|m| m.model == old.model && m.id == old.id))
            {
                stamp(
                    tx,
                    descriptor,
                    old.id,
                    old.revision
                        .checked_add(1)
                        .ok_or_else(|| invalid("revision overflow"))?,
                    &self.scope,
                )
                .await?;
                let next = self.entity(tx, descriptor, old.id).await?;
                self.policy
                    .validate(&self.principal, &self.scope, old, &next)?;
                self.policy.after_write(tx, old, &next).await?;
                if self.model_logs {
                    crate::model_log::append(tx, &self.registry, old, &next).await?;
                }
                affected.push(next);
            }
        }
        for entity in &affected {
            if let Some(record) = &entity.record {
                self.check_references(tx, self.registry.get(&entity.model)?, record, mutations)
                    .await?;
            }
        }
        if self.model_logs {
            // A canonical no-op result acknowledges normalization without a new
            // revision, journal event or lifecycle effect.
            for mutation in mutations {
                if !affected
                    .iter()
                    .any(|entity| entity.model == mutation.model && entity.id == mutation.id)
                {
                    let old = before
                        .iter()
                        .find(|old| old.model == mutation.model && old.id == mutation.id)
                        .ok_or_else(|| invalid("missing no-op baseline"))?;
                    self.policy
                        .validate(&self.principal, &self.scope, old, old)?;
                    affected.push(old.clone());
                }
            }
            return Ok(affected);
        }
        tx.execute_sql(
            "UPDATE _sync_scopes SET sequence = sequence + 1 WHERE scope = $1",
            vec![text(&self.scope)],
        )
        .await?;
        let cursor = self.cursor(tx).await?;
        let group = ChangeGroup {
            sequence: cursor.sequence,
            entities: affected.clone(),
        };
        tx.execute_sql(
            "INSERT INTO _sync_changes (scope, sequence, payload) VALUES ($1,$2,$3)",
            vec![
                text(&self.scope),
                D::Integer(cursor.sequence),
                text(encode(&group)?),
            ],
        )
        .await?;
        Ok(affected)
    }
}

impl SyncServer {
    async fn external_write(
        &self,
        tx: &Transaction,
        ast: &che_orm::QueryAst,
    ) -> Result<che_orm::ModelWriteResult, OrmError> {
        let before = self.inventory(tx).await?;
        let compiled = che_orm::Compiler::compile(ast);
        let returning = match ast {
            che_orm::QueryAst::Insert(ast) => !ast.returning.is_empty(),
            che_orm::QueryAst::Update(ast) => !ast.returning.is_empty(),
            _ => false,
        };
        let (result, rows) = if returning {
            let rows = tx.query_sql(compiled.sql, compiled.params).await?;
            (
                che_orm::ExecuteResult {
                    rows_affected: rows.len(),
                    last_insert_rowid: None,
                },
                rows,
            )
        } else {
            (tx.execute_sql(compiled.sql, compiled.params).await?, vec![])
        };
        for old in before.into_iter().filter(|entity| entity.record.is_some()) {
            let descriptor = self.registry.get(&old.model)?;
            let record = read(tx, descriptor, old.id, false).await?;
            if record == old.record {
                continue;
            }
            let revision = old
                .revision
                .checked_add(1)
                .ok_or_else(|| invalid("revision overflow"))?;
            let next = if record.is_some() {
                stamp(tx, descriptor, old.id, revision, &self.scope).await?;
                self.entity(tx, descriptor, old.id).await?
            } else {
                let now = che_orm::time::OffsetDateTime::now_utc();
                Entity {
                    revision,
                    updated_at: Some(now),
                    deleted_at: Some(now),
                    record: None,
                    ..old.clone()
                }
            };
            self.policy
                .validate(&self.principal, &self.scope, &old, &next)?;
            self.policy.after_write(tx, &old, &next).await?;
            crate::model_log::append(tx, &self.registry, &old, &next).await?;
        }
        Ok(che_orm::ModelWriteResult { result, rows })
    }
}

async fn stamp(
    tx: &Transaction,
    d: &Descriptor,
    id: Uuid,
    revision: i64,
    scope: &str,
) -> Result<(), OrmError> {
    tx.execute_sql(
        format!(
            "UPDATE {} SET {} = $1, {} = $2, {} = $3 WHERE {} = $4",
            quote(d.schema.name),
            quote(d.revision),
            quote(d.updated_at),
            quote(d.scope),
            quote(d.primary)
        ),
        vec![
            D::Integer(revision),
            D::DateTime(che_orm::time::OffsetDateTime::now_utc()),
            text(scope),
            D::Uuid(id),
        ],
    )
    .await?;
    Ok(())
}

fn soft_delete<'a>(
    tx: &'a Transaction,
    registry: &'a Registry,
    d: &'a Descriptor,
    id: Uuid,
) -> che_orm::TransactionFuture<'a, ()> {
    Box::pin(async move {
        for child in registry.models.iter() {
            for column in &child.schema.columns {
                let Some(fk) = &column.references else {
                    continue;
                };
                if fk.target_parts().unwrap().0 != d.schema.name {
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
                for row in rows {
                    let child_id = row.get::<Uuid>(0)?;
                    match fk
                        .on_delete()
                        .unwrap_or("restrict")
                        .to_ascii_lowercase()
                        .as_str()
                    {
                        "cascade" => soft_delete(tx, registry, child, child_id).await?,
                        "set null" => {
                            tx.execute_sql(
                                format!(
                                    "UPDATE {} SET {} = NULL WHERE {} = $1",
                                    quote(child.schema.name),
                                    quote(column.name),
                                    quote(child.primary)
                                ),
                                vec![D::Uuid(child_id)],
                            )
                            .await?;
                        }
                        _ => return Err(invalid("delete restricted by related records")),
                    }
                }
            }
        }
        tx.execute_sql(
            format!(
                "UPDATE {} SET {} = $1 WHERE {} = $2",
                quote(d.schema.name),
                quote(d.deleted_at),
                quote(d.primary)
            ),
            vec![
                D::DateTime(che_orm::time::OffsetDateTime::now_utc()),
                D::Uuid(id),
            ],
        )
        .await?;
        Ok(())
    })
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
                            "SELECT {} FROM {} WHERE {} = $1 AND {} IS NULL",
                            quote(child.primary),
                            quote(child.schema.name),
                            quote(column.name),
                            quote(child.deleted_at)
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
        values.push((d.revision, D::Integer(0)));
        values.push((d.scope, text("")));
        values.push((d.deleted_at, D::Null));
        if !values.iter().any(|(column, _)| *column == d.updated_at) {
            values.push((d.updated_at, D::DateTime(now)));
        }
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
