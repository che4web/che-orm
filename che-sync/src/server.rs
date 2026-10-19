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
    orm_db: Database,
    registry: Registry,
    scope: String,
    principal: String,
    policy: Arc<dyn AccessPolicy>,
    model: Option<String>,
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
        };
        server.orm_db = server
            .db
            .clone()
            .with_model_runtime(Arc::new(server.clone()));
        Ok(server)
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
        self.push_transaction(request, trusted, None, vec![], None)
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
        self.push_transaction(request, false, Some(before), vec![], None)
            .await
    }

    pub async fn push_with_effects(
        &self,
        request: PushRequest,
        before: Vec<Entity>,
        effects: Vec<PreparedEffect>,
    ) -> Result<(PushResult, bool), OrmError> {
        self.push_transaction(request, false, Some(before), effects, None)
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

    /// Replay REST receipts before serializer preparation or loading a deleted row.
    pub async fn rest_receipt(
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

    pub async fn write_rest_prepared(
        &self,
        request: PushRequest,
        before: Vec<Entity>,
        effects: Vec<PreparedEffect>,
        identity: String,
    ) -> Result<PushResult, OrmError> {
        self.push_transaction(request, true, Some(before), effects, Some(identity))
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
        rest_identity: Option<String>,
    ) -> Result<(PushResult, bool), OrmError> {
        self.policy.authorize_scope(&self.principal, &self.scope)?;
        if request.protocol_version != PROTOCOL_VERSION || request.scope != self.scope {
            return Err(invalid("incompatible push protocol or scope"));
        }
        if request.operation.mutations.is_empty() {
            return Err(invalid("empty operation"));
        }
        if let Some(model) = &self.model {
            if request
                .operation
                .mutations
                .iter()
                .any(|m| &m.model != model)
            {
                return Err(invalid("operation contains a model outside this feed"));
            }
        }
        let server = self.clone();
        // READ COMMITTED plus a scope row lock yields commit-ordered groups in
        // PostgreSQL. SQLite's write lock provides the same serialization.
        self.db.transaction_with_isolation(IsolationLevel::ReadCommitted, move |tx| Box::pin(async move {
            tx.execute_sql("UPDATE _sync_scopes SET sequence = sequence WHERE scope = $1", vec![text(&server.scope)]).await?;
            let request_json = match &rest_identity { Some(identity) => encode(&(identity, &request))?, None => encode(&request)? };
            let receipt_replica = format!("{}:{}", server.principal.len(), server.principal) + &request.replica_id.to_string();
            let rows = tx.query_sql("SELECT request, result FROM _sync_receipts WHERE scope = $1 AND replica_id = $2 AND operation_id = $3", vec![text(&server.scope), text(&receipt_replica), text(request.operation.id)]).await?;
            if let Some(row) = rows.first() {
                let same = match &rest_identity {
                    Some(identity) => decode::<(String, PushRequest)>(&row.get::<String>(0)?)?.0 == *identity,
                    None => decode::<PushRequest>(&row.get::<String>(0)?)? == request,
                };
                if !same { return Err(invalid("operation ID reused with different contents")); }
                return Ok((decode(&row.get::<String>(1)?)?, false));
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
                    if entity.revision == 0 && m.changes.is_none() { return Err(invalid("cannot delete an unknown UUID")); }
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
                        if v.column.name == d.primary {
                            if let D::Uuid(id) = v.value {
                                return Some(id);
                            }
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
                    base_revision: old.revision,
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

    async fn entity(&self, tx: &Transaction, d: &Descriptor, id: Uuid) -> Result<Entity, OrmError> {
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
        if let che_orm::QueryAst::Select(select) = ast {
            if std::iter::once(select.table.name)
                .chain(select.joins.iter().map(|join| join.table.name))
                .any(|table| self.registry.models.iter().any(|d| d.schema.name == table))
            {
                self.policy.authorize_scope(&self.principal, &self.scope)?;
            }
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
            mutations.push(Mutation {
                model: d.name.into(),
                id: *id,
                schema_version: d.version,
                base_revision: old.revision,
                changes,
            });
            before.push(old);
        }
        for mutation in mutations.iter().filter(|m| m.changes.is_none()) {
            collect_dependents(tx, self, d.name, mutation.id, &mut before).await?;
        }
        self.apply_and_journal(tx, &mutations, &before).await?;
        crate::runtime::result(tx, d, ast, &ids).await
    }

    /// Shared transactional application for REST/native requests and ORM writes.
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
                if mutation.changes.is_some() {
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
                || mutations
                    .iter()
                    .any(|m| m.model == old.model && m.id == old.id)
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
                affected.push(next);
            }
        }
        for entity in &affected {
            if let Some(record) = &entity.record {
                self.check_references(tx, self.registry.get(&entity.model)?, record, mutations)
                    .await?;
            }
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
