//! Offline planning for newly generated migrations. Historical operations and
//! checksums are inputs, never regenerated from the current model definitions.
use super::catalog::{Catalog, has_key, physical_key, same_name};
use super::*;

mod operation_plan;
use operation_plan::{OperationPlan, access};

/// File ownership for generated migrations. Model ownership is always retained.
#[derive(Clone, Copy, Debug)]
pub enum MigrationLayout<'a> {
    /// Store every operation under the named application's migration history.
    Single(&'a str),
    /// Store operations in the history of the application that owns the model.
    PerApp,
}

impl<'a> MigrationLayout<'a> {
    fn file_owner<'b>(self, owner: &'b str) -> &'b str
    where
        'a: 'b,
    {
        match self {
            Self::Single(app) => app,
            Self::PerApp => owner,
        }
    }
}

impl MigrationGraph {
    fn next_id(&self, app: &str, name: &str) -> Result<MigrationId, MigrationError> {
        if app.is_empty()
            || name.is_empty()
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(invalid("invalid generated migration name"));
        }
        let mut number = 0u32;
        for m in self.ordered().filter(|m| m.id.app == app) {
            let prefix = m.id.name.split('_').next().unwrap_or("");
            number = number.max(prefix.parse().map_err(|_| {
                invalid(format!(
                    "non-numeric migration prefix: {}:{}",
                    app, m.id.name
                ))
            })?);
        }
        let number = number
            .checked_add(1)
            .ok_or_else(|| invalid("migration number overflow"))?;
        Ok(MigrationId {
            app: app.into(),
            name: format!("{number:04}_{name}"),
        })
    }

    /// Creates an editable empty file, or joins independent branches of one app.
    pub fn manual_migration(
        &self,
        app: &str,
        name: &str,
        merge: bool,
    ) -> Result<Migration, MigrationError> {
        let heads = self.app_heads(app);
        if merge {
            if heads.len() < 2 {
                return Err(invalid(format!("no conflicting heads in {app}")));
            }
            self.validate_merge(&heads)?;
        } else {
            self.validate_heads()?;
        }
        Ok(Migration::new(
            self.next_id(app, name)?,
            heads.into_iter().map(|m| m.id.clone()).collect(),
            vec![],
        ))
    }

    fn validate_merge(&self, heads: &[&Migration]) -> Result<(), MigrationError> {
        let closures = heads
            .iter()
            .map(|m| self.target_closure(&m.id))
            .collect::<Result<Vec<_>, _>>()?;
        let selected = closures.iter().flatten().cloned().collect();
        let state = self
            .replay_selected(ProjectState::default(), &selected)
            .map_err(|error| {
                let branches = heads
                    .iter()
                    .map(|m| format!("{}:{}", m.id.app, m.id.name))
                    .collect::<Vec<_>>();
                invalid(format!("cannot merge {}: {error}", branches.join(", ")))
            })?;
        let policy = GenerationPolicy::default();
        let catalog = Catalog::new(&state, &state, &policy)?;
        let mut cursor = ProjectState::default();
        let mut branches = Vec::new();
        for migration in self.ordered().filter(|m| selected.contains(&m.id)) {
            let membership: BTreeSet<_> = closures
                .iter()
                .enumerate()
                .filter_map(|(i, closure)| closure.contains(&migration.id).then_some(i))
                .collect();
            for op in &migration.operations {
                let touched = access(std::slice::from_ref(op), &mut cursor, &catalog)?;
                for (other_heads, other, previous, id) in &branches {
                    if membership.is_subset(other_heads) || membership.is_superset(other_heads) {
                        continue;
                    }
                    if touched.conflicts(other) && !independent_added_columns(previous, op) {
                        return Err(invalid(format!(
                            "merge requires review: {id:?} ({}) conflicts with {:?} ({})",
                            previous.summary(),
                            migration.id,
                            op.summary()
                        )));
                    }
                }
                branches.push((
                    membership.clone(),
                    touched,
                    op.clone(),
                    migration.id.clone(),
                ));
            }
        }
        Ok(())
    }

    /// Renders the selected backend's forward SQL against the target's own ancestors.
    pub fn migration_sql(&self, target: &MigrationId) -> Result<Vec<String>, MigrationError> {
        let mut ancestors = self.target_closure(target)?;
        ancestors.remove(target);
        let before = self.replay_selected(ProjectState::default(), &ancestors)?;
        let operations = &self.migrations[target].operations;
        #[cfg(feature = "sqlite")]
        {
            let mut state = before;
            let mut sql = Vec::new();
            let mut physical = PhysicalOperations::new(operations);
            while let Some(op) = physical.next(&state) {
                let plan = SqliteSchemaEditor::plan(op, &state)?;
                state = plan.resulting_state;
                sql.extend(plan.statements);
            }
            Ok(sql)
        }
        #[cfg(feature = "postgres")]
        {
            let mut after = before.clone();
            for op in operations {
                op.state_forwards(&mut after)?;
            }
            let external = after
                .tables()
                .iter()
                .flat_map(|t| targets(&t.columns))
                .filter(|(name, _)| !after.tables().iter().any(|t| t.name == *name))
                .map(|(name, _)| name.to_owned())
                .collect();
            Ok(
                PostgresSchemaEditor::plan_with_external_targets(operations, &before, &external)?
                    .statements,
            )
        }
    }

    /// Renders a script suitable for executing as one batch. A delimiter on
    /// its own line also terminates SQL ending in a line comment.
    pub fn migration_sql_script(&self, target: &MigrationId) -> Result<String, MigrationError> {
        let mut script = String::new();
        for statement in self.migration_sql(target)? {
            script.push_str(&statement);
            script.push_str("\n;\n");
        }
        Ok(script)
    }

    /// Plans initial files against existing history. For mutually dependent
    /// new models, it can install tables before adding cross-app foreign keys.
    pub fn generate_initial(
        &self,
        desired: &ProjectState,
        label: &str,
    ) -> Result<Vec<Migration>, MigrationError> {
        let historical = self.replay(ProjectState::default())?;
        let layout = MigrationLayout::PerApp;
        let error = match self.generate(desired, layout, label) {
            Ok(files) => return Ok(files),
            Err(error) => error,
        };

        // A cycle between new models can be broken by installing those tables
        // before their cross-app foreign keys. Both phases use the regular
        // planner, including ordering and dependencies for SQL objects.
        let Some(staged) = defer_new_foreign_keys(&historical, desired) else {
            return Err(error);
        };
        let mut files = self.generate_plan(
            &staged,
            layout,
            label,
            &GenerationPolicy::default(),
            Some(desired),
        )?;
        let base = MigrationGraph::new(
            self.ordered()
                .cloned()
                .chain(files.iter().cloned())
                .collect(),
        )?;
        files.extend(base.generate(desired, layout, "relationships")?);
        Ok(files)
    }

    /// Plans one application's changes against the compiled history of other
    /// apps. Pending consumers in the registry determine the file boundaries,
    /// so generating a provider first can produce several migrations.
    pub fn generate_for_app(
        &self,
        desired: &ProjectState,
        app: &str,
        name: &str,
        policy: &GenerationPolicy,
    ) -> Result<Vec<Migration>, MigrationError> {
        let historical = self.replay(ProjectState::default())?;
        let mut selected = historical.clone();
        selected.tables.retain(|table| {
            selected
                .model_keys
                .get(&table.name)
                .is_none_or(|key| key.app != app)
        });
        selected.model_keys.retain(|_, key| key.app != app);
        selected.objects.retain(|object| object.app != app);
        selected.requirements.retain(|(kind, name), _| {
            selected
                .objects
                .iter()
                .any(|o| o.kind == *kind && o.name == *name)
        });
        for model in desired
            .models()
            .into_iter()
            .filter(|model| model.key.app == app)
        {
            for (target, _) in targets(&model.table.columns) {
                if let Some(owner) = table_owner(desired, target)
                    && owner != app
                    && table_owner(&selected, target) != Some(owner)
                {
                    return Err(invalid(format!(
                        "foreign key on {} needs a migration for application {owner}; generate and register it first",
                        model.table.name
                    )));
                }
            }
            MigrationOperation::CreateModel { model }.state_forwards(&mut selected)?;
        }
        for object in desired.objects().iter().filter(|object| object.app == app) {
            if let Some(requires) = desired.object_requirements(object) {
                selected.add_declared_object(object.clone(), requires.iter().cloned())?;
            } else {
                selected.add_schema_object(object.clone())?;
            }
            if historical.objects().contains(object)
                && historical.object_requirements(object) == desired.object_requirements(object)
            {
                continue;
            }
            let reads = sql_dependencies::SqlReads::new(object);
            for table in desired.tables().iter().map(|table| &table.name).chain(
                desired
                    .objects()
                    .iter()
                    .filter(|provider| provider.kind == SchemaObjectKind::Table)
                    .map(|provider| &provider.name),
            ) {
                if reads.references(table)
                    && !selected
                        .tables()
                        .iter()
                        .any(|candidate| candidate.name == *table)
                    && !selected.objects().iter().any(|candidate| {
                        candidate.kind == SchemaObjectKind::Table && candidate.name == *table
                    })
                    && !desired.objects().iter().any(|candidate| {
                        candidate.app == app
                            && candidate.kind == SchemaObjectKind::Table
                            && candidate.name == *table
                    })
                {
                    return Err(invalid(format!(
                        "schema object {} requires table {table}; generate and register its migration first",
                        object.name
                    )));
                }
            }
            operation_plan::Access::check_scoped_object(object, &selected, desired, policy)?;
            for provider in desired
                .objects()
                .iter()
                .filter(|provider| provider.kind == SchemaObjectKind::View && provider.app != app)
            {
                if !desired.requires_object(object, provider, false) {
                    continue;
                }
                let registered = selected.objects().iter().find(|candidate| {
                    candidate.kind == SchemaObjectKind::View
                        && same_name(&candidate.name, &provider.name)
                });
                // SQLite can resolve a consumer view against the registered
                // definition below, even if its provider has pending edits.
                if registered.is_none()
                    || (registered != Some(provider)
                        && !historical.objects().contains(object)
                        && !(cfg!(feature = "sqlite") && object.kind == SchemaObjectKind::View))
                {
                    return Err(invalid(format!(
                        "schema object {} requires view {}; generate and register its migration first",
                        object.name, provider.name
                    )));
                }
            }
        }
        #[cfg(feature = "sqlite")]
        validate_scoped_view_columns(&selected, desired, app)?;
        self.generate_plan(
            &selected,
            MigrationLayout::Single(app),
            name,
            policy,
            Some(desired),
        )
    }

    /// Generate only new migrations, in application order, without I/O. Each
    /// file is checked with the selected backend's schema editor. This checks
    /// structural prerequisites, not constraints on existing row values.
    pub fn generate(
        &self,
        desired: &ProjectState,
        layout: MigrationLayout<'_>,
        name: &str,
    ) -> Result<Vec<Migration>, MigrationError> {
        self.generate_with_policy(desired, layout, name, &GenerationPolicy::default())
    }

    /// Plan using application-declared SQL resources, external requirements
    /// and explicit authorizations for destructive changes.
    pub fn generate_with_policy(
        &self,
        desired: &ProjectState,
        layout: MigrationLayout<'_>,
        name: &str,
        policy: &GenerationPolicy,
    ) -> Result<Vec<Migration>, MigrationError> {
        self.generate_plan(desired, layout, name, policy, None)
    }

    fn generate_plan(
        &self,
        desired: &ProjectState,
        layout: MigrationLayout<'_>,
        name: &str,
        policy: &GenerationPolicy,
        registered: Option<&ProjectState>,
    ) -> Result<Vec<Migration>, MigrationError> {
        self.validate_heads()?;
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(invalid("invalid generated migration name"));
        }
        let historical = self.replay(ProjectState::default())?;
        let mut origins = SqliteIndexOrigins::default();
        for file in self.ordered() {
            origins.observe(&file.operations);
        }
        let mut operations = Vec::new();
        let mut selected_policy = policy.clone();
        if let (Some(_), MigrationLayout::Single(app)) = (registered, layout) {
            selected_policy
                .rename_models
                .retain(|(owner, _), _| owner == app);
            selected_policy.rename_columns.retain(|(table, _), _| {
                table_owner(&historical, table).is_none_or(|owner| owner == app)
            });
            selected_policy.sql_tables.retain(|table, _| {
                [desired, &historical].iter().any(|s| {
                    s.objects()
                        .iter()
                        .any(|o| o.kind == SchemaObjectKind::Table && o.name == *table)
                })
            });
        }
        let catalog = Catalog::new(&historical, desired, &selected_policy)?;
        collect_operations(
            &historical,
            layout,
            &catalog,
            &mut origins,
            &mut operations,
            true,
        )?;
        let emitted = operations.len();
        if emitted == 0 {
            return Ok(vec![]);
        }
        let Some(registry) = registered else {
            return OperationPlan::new(self, operations, &catalog, emitted)?
                .migrations(self, historical, name, &catalog);
        };
        // Include the rest of the project's transitions so indirect consumers
        // also determine the file boundaries of the emitted provider operations.
        let mut remaining_policy = policy.clone();
        remaining_policy
            .rename_models
            .retain(|k, _| !selected_policy.rename_models.contains_key(k));
        remaining_policy
            .rename_columns
            .retain(|k, _| !selected_policy.rename_columns.contains_key(k));
        // A changed opaque table needs a hand-written conversion. Its compiled
        // definition remains the provider for independent scoped generation.
        let mut registry = std::borrow::Cow::Borrowed(registry);
        let unresolved: Vec<_> = desired
            .objects()
            .iter()
            .filter(|old| old.kind == SchemaObjectKind::Table)
            .filter_map(|old| {
                registry
                    .objects()
                    .iter()
                    .position(|o| o.kind == old.kind && o.name == old.name && o != old)
                    .map(|i| (i, old))
            })
            .collect();
        if !unresolved.is_empty() {
            let mut cursor = historical.clone();
            for (_, op) in &operations {
                let resources = access(std::slice::from_ref(op), &mut cursor, &catalog)?;
                if let Some((_, old)) = unresolved
                    .iter()
                    .find(|(_, old)| resources.requires_table(&old.name))
                {
                    return Err(MigrationError::NeedsManualMigration {
                        resource: old.name.clone(),
                        reason: "register the required SQL table conversion before generating its consumer".into(),
                    });
                }
            }
            for (i, old) in unresolved {
                registry.to_mut().objects[i] = old.clone();
            }
        }
        let full_catalog = Catalog::new(&historical, &registry, &remaining_policy)?;
        collect_operations(
            desired,
            MigrationLayout::PerApp,
            &full_catalog,
            &mut origins,
            &mut operations,
            false,
        )?;
        OperationPlan::new(self, operations, &full_catalog, emitted)?
            .migrations(self, historical, name, &catalog)
    }

    fn validate_generated_history(
        &self,
        graph: &MigrationGraph,
        desired: &ProjectState,
        catalog: &Catalog<'_>,
    ) -> Result<(), MigrationError> {
        graph.validate_heads()?;
        // The graph may order independent files differently than the generator.
        // Verify that exact execution order as well; do not reinterpret old SQL.
        let mut replayed = ProjectState::default();
        for file in graph.ordered() {
            if self.migrations.contains_key(&file.id) {
                for op in &file.operations {
                    op.state_forwards(&mut replayed)?;
                }
            } else {
                replayed = validate_file(&replayed, &file.operations, catalog)?;
            }
        }
        if !replayed.diff(desired)?.is_empty() {
            return Err(invalid("generated history differs from registered schema"));
        }
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
fn validate_scoped_view_columns(
    state: &ProjectState,
    desired: &ProjectState,
    app: &str,
) -> Result<(), MigrationError> {
    if !state
        .objects()
        .iter()
        .any(|o| o.kind == SchemaObjectKind::View && o.app != app)
    {
        return Ok(());
    }
    for view in state
        .objects()
        .iter()
        .filter(|o| o.kind == SchemaObjectKind::View && o.app == app)
    {
        let sources = view_sources(state, [view]);
        let mut providers = sources
            .iter()
            .filter(|o| o.kind == SchemaObjectKind::View && o.app != app)
            .peekable();
        if providers.peek().is_none() {
            continue;
        }
        let pending_provider = providers.any(|registered| {
            desired.objects().iter().any(|o| {
                o.kind == registered.kind && same_name(&o.name, &registered.name) && o != registered
            })
        });
        let db = view_catalog(state, sources)?;
        if let Err(error) = prepare_view(&db, view) {
            // Offline generation can encounter application-defined SQL functions
            // and external tables. If a provider is changing, its registered
            // exports must be usable before creating this consumer on its own.
            let external = matches!(&error,
                crate::rusqlite::Error::SqliteFailure(_, Some(message))
                | crate::rusqlite::Error::SqlInputError { msg: message, .. }
                if ["no such table:", "no such function:", "no such collation sequence:", "no such module:"]
                    .iter().any(|prefix| message.starts_with(prefix))
            );
            if !external || pending_provider {
                return Err(invalid(format!(
                    "view {} cannot use the registered schema: {error}; generate and register its provider migration first",
                    view.name
                )));
            }
        }
    }
    Ok(())
}

#[cfg(feature = "sqlite")]
fn view_sources<'a>(
    state: &'a ProjectState,
    views: impl IntoIterator<Item = &'a SchemaObjectState>,
) -> Vec<SchemaObjectState> {
    let mut pending: Vec<_> = views.into_iter().collect();
    let mut required = BTreeMap::new();
    while let Some(object) = pending.pop() {
        if required
            .insert((object.kind, &object.name), object)
            .is_none()
        {
            pending.extend(
                state
                    .objects()
                    .iter()
                    .filter(|source| state.requires_object(object, source, false)),
            );
        }
    }
    required.into_values().cloned().collect()
}

// Preparing a SELECT on an in-memory schema resolves SQLite's view exports
// and nested projections without querying the application's database.
#[cfg(feature = "sqlite")]
fn view_catalog(
    desired: &ProjectState,
    sources: Vec<SchemaObjectState>,
) -> Result<crate::rusqlite::Connection, MigrationError> {
    let objects = order_schema_objects(sources, false, desired)?;
    let open = || -> crate::rusqlite::Result<_> {
        let db = crate::rusqlite::Connection::open_in_memory()?;
        db.set_db_config(
            crate::rusqlite::config::DbConfig::SQLITE_DBCONFIG_DQS_DML,
            false,
        )?;
        for table in desired.tables() {
            let columns = table
                .columns
                .iter()
                .map(|c| quote_identifier(&c.name))
                .collect::<Vec<_>>()
                .join(", ");
            db.execute(
                &format!("CREATE TABLE {} ({columns})", quote_identifier(&table.name)),
                [],
            )?;
        }
        for object in objects {
            db.execute(&object.sql, [])?;
        }
        Ok(db)
    };
    open().map_err(|e| invalid(format!("cannot prepare final view catalog: {e}")))
}

#[cfg(feature = "sqlite")]
fn prepare_view(
    db: &crate::rusqlite::Connection,
    view: &SchemaObjectState,
) -> crate::rusqlite::Result<()> {
    db.prepare(&format!(
        "SELECT * FROM {} LIMIT 0",
        quote_identifier(&view.name)
    ))
    .map(|_| ())
}

#[cfg(feature = "sqlite")]
fn unresolved_column(error: &crate::rusqlite::Error) -> bool {
    matches!(error,
        crate::rusqlite::Error::SqliteFailure(_, Some(message))
        | crate::rusqlite::Error::SqlInputError { msg: message, .. }
        if message.starts_with("no such column:") || message.starts_with("ambiguous column name:")
    )
}

fn validate_final_column_references(
    desired: &ProjectState,
    table: &str,
    column: &str,
) -> Result<(), MigrationError> {
    let mut pending = vec![table];
    let mut visited = BTreeSet::new();
    #[cfg(feature = "sqlite")]
    let mut catalog = None;
    while let Some(source) = pending.pop() {
        if !visited.insert(source) {
            continue;
        }
        for object in desired
            .objects()
            .iter()
            .filter(|o| sql_dependencies::references(o, source))
        {
            if object.kind == SchemaObjectKind::View {
                #[cfg(feature = "postgres")]
                let propagate = !sql_dependencies::exports_alias(&object.sql, column);
                #[cfg(feature = "sqlite")]
                let propagate = true;
                if propagate {
                    pending.push(&object.name);
                }
            }
            // SQLite resolves each affected view even when it only uses the
            // name of an output column from another view.
            let needs_review = sql_dependencies::may_read_column(object, source, column, desired);
            #[cfg(feature = "sqlite")]
            let needs_review = if object.kind == SchemaObjectKind::View {
                let db = match &catalog {
                    Some(db) => db,
                    None => catalog.insert(view_catalog(
                        desired,
                        view_sources(
                            desired,
                            desired
                                .objects()
                                .iter()
                                .filter(|o| o.kind == SchemaObjectKind::View),
                        ),
                    )?),
                };
                match prepare_view(db, object) {
                    Ok(()) => continue,
                    Err(error) if unresolved_column(&error) => true,
                    Err(_) => needs_review,
                }
            } else {
                needs_review
            };
            if !needs_review {
                continue;
            }
            return Err(MigrationError::NeedsManualMigration {
                resource: object.name.clone(),
                reason: format!(
                    "review the final SQL referencing {table}.{column} through {source} before changing the column"
                ),
            });
        }
    }
    Ok(())
}

// Drop consumers before their providers. The ordinary diff reinstalls the
// desired definitions once the model changes are complete.
fn rename_object_drops(
    state: &ProjectState,
    desired: &ProjectState,
    policy: &GenerationPolicy,
) -> Result<Vec<StateChange>, MigrationError> {
    let mut affected = BTreeMap::new();
    for (table, column) in policy.rename_columns.keys() {
        // Validate the SQL that will actually be installed, including edited
        // and newly registered definitions. AS old_name can preserve a view's
        // public column name without reading the removed physical column.
        let final_table = state
            .model_keys
            .get(table)
            .and_then(|key| {
                desired
                    .model_keys
                    .iter()
                    .find(|(_, candidate)| *candidate == key)
                    .map(|(name, _)| name)
            })
            .unwrap_or(table);
        validate_final_column_references(desired, final_table, column)?;
        let mut relations = BTreeSet::from([table.clone()]);
        loop {
            let mut added = false;
            for object in state.objects() {
                if !relations
                    .iter()
                    .any(|name| sql_dependencies::references(object, name))
                {
                    continue;
                }
                if object.kind == SchemaObjectKind::Table {
                    if sql_dependencies::mentions_identifier(&object.sql, column) {
                        return Err(MigrationError::NeedsManualMigration {
                            resource: object.name.clone(),
                            reason: format!(
                                "review SQL table referencing {table}.{column} before renaming the column"
                            ),
                        });
                    }
                    continue;
                }
                affected.insert((object.kind, object.name.clone()), object.clone());
                if object.kind == SchemaObjectKind::View {
                    added |= relations.insert(object.name.clone());
                }
            }
            if !added {
                break;
            }
        }
    }
    Ok(
        order_schema_objects(affected.into_values().collect(), true, state)?
            .into_iter()
            .map(StateChange::DeleteSchemaObject)
            .collect(),
    )
}

/// Origin of the *actual* SQLite index names after bare historical renames.
/// A second rename carries the original prefix; a table rebuild recreates the
/// indexes with the current prefix. This metadata never enters a checksum.
#[derive(Default)]
struct SqliteIndexOrigins(BTreeMap<String, String>);

impl SqliteIndexOrigins {
    fn observe(&mut self, ops: &[MigrationOperation]) {
        use MigrationOperation::*;
        for op in ops {
            match op {
                RenameTable { old_name, new_name } => {
                    let origin = self.0.remove(old_name).unwrap_or_else(|| old_name.clone());
                    if origin != *new_name {
                        self.0.insert(new_name.clone(), origin);
                    }
                }
                RenameSqliteIndexes {
                    table, to_table, ..
                } if table != to_table => {
                    self.0.insert(table.clone(), to_table.clone());
                }
                DropTable { name } | RenameSqliteIndexes { table: name, .. } => {
                    self.0.remove(name);
                }
                _ => {
                    if let Some(table) = SqliteSchemaEditor::rebuild_table_name(op) {
                        self.0.remove(table);
                    }
                }
            }
        }
    }

    fn repairs(
        &self,
        history: &ProjectState,
        desired: &ProjectState,
    ) -> Result<Vec<(String, MigrationOperation)>, MigrationError> {
        if self.0.is_empty() || history.diff(desired)?.is_empty() {
            return Ok(vec![]);
        }
        let mut wanted = BTreeSet::new();
        for table in desired.tables() {
            wanted.insert(table.name.clone());
            wanted.extend((0..table.indexes.len()).map(|i| index_name(&table.name, i)));
        }
        wanted.extend(
            desired
                .objects()
                .iter()
                .filter(|o| o.kind != SchemaObjectKind::Trigger)
                .map(|o| o.name.to_ascii_lowercase()),
        );
        let mut repairs = Vec::new();
        for schema in history.tables() {
            let table = &schema.name;
            let Some(from_table) = self.0.get(table) else {
                continue;
            };
            if (0..schema.indexes.len()).any(|i| wanted.contains(&index_name(from_table, i))) {
                let owner = history
                    .model_keys
                    .get(table)
                    .ok_or_else(|| invalid(format!("missing migration owner for {table}")))?;
                repairs.push((
                    owner.app.clone(),
                    MigrationOperation::RenameSqliteIndexes {
                        table: table.clone(),
                        from_table: from_table.clone(),
                        to_table: table.clone(),
                    },
                ));
            }
        }
        Ok(repairs)
    }
}

/// Allow new models on opposite sides of a cross-app cycle to be installed
/// before their relationships. Views and triggers follow the complete tables.
fn defer_new_foreign_keys(
    historical: &ProjectState,
    desired: &ProjectState,
) -> Option<ProjectState> {
    let mut staged = desired.clone();
    let mut deferred = false;
    for table in &mut staged.tables {
        let Some(owner) = staged.model_keys.get(&table.name) else {
            continue;
        };
        if historical.model_keys.contains_key(&table.name) {
            continue;
        }
        for column in &mut table.columns {
            if let Some((target, _)) = column
                .references
                .as_ref()
                .and_then(ForeignKeyState::target_parts)
                && staged
                    .model_keys
                    .get(target)
                    .is_some_and(|provider| provider.app != owner.app)
                && !historical.model_keys.contains_key(target)
            {
                column.references = None;
                deferred = true;
            }
        }
    }
    if !deferred {
        return None;
    }
    staged
        .objects
        .retain(|object| object.kind == SchemaObjectKind::Table);
    staged.objects.extend(
        historical
            .objects
            .iter()
            .filter(|object| object.kind != SchemaObjectKind::Table)
            .cloned(),
    );
    Some(staged)
}

/// Collect actual schema transitions without assigning files. Intent checks
/// authorize only the operations that the current command can emit.
fn collect_operations(
    historical: &ProjectState,
    layout: MigrationLayout<'_>,
    catalog: &Catalog<'_>,
    origins: &mut SqliteIndexOrigins,
    operations: &mut Vec<(String, MigrationOperation)>,
    emitting: bool,
) -> Result<(), MigrationError> {
    let desired = catalog.desired;
    let policy = catalog.policy;
    let mut state = historical.clone();
    for (owner, op) in origins.repairs(historical, desired)? {
        op.state_forwards(&mut state)?;
        origins.observe(std::slice::from_ref(&op));
        operations.push((layout.file_owner(&owner).to_owned(), op));
    }
    for ((app, old_name), new_name) in &policy.rename_models {
        let old = ModelKey {
            app: app.clone(),
            name: old_name.clone(),
        };
        let new = ModelKey {
            app: app.clone(),
            name: new_name.clone(),
        };
        let table = state
            .model_keys
            .iter()
            .find(|(_, key)| **key == old)
            .ok_or_else(|| invalid(format!("unknown historical model {app}.{old_name}")))?
            .0
            .clone();
        if !desired.model_keys.values().any(|key| *key == new)
            || desired.model_keys.values().any(|key| *key == old)
        {
            return Err(invalid(format!(
                "expected {app}.{new_name} to replace {app}.{old_name} in the model registry"
            )));
        }
        let op = MigrationOperation::RenameModel {
            table,
            app: app.clone(),
            old_name: old_name.clone(),
            new_name: new_name.clone(),
        };
        op.state_forwards(&mut state)?;
        operations.push((layout.file_owner(app).to_owned(), op));
    }
    let mut explicit_changes = rename_object_drops(&state, desired, policy)?;
    for ((table, old_name), new_name) in &policy.rename_columns {
        let key = state
            .model_keys
            .get(table)
            .ok_or_else(|| invalid(format!("unknown historical table {table}")))?;
        let model = desired
            .models()
            .into_iter()
            .find(|m| m.key == *key)
            .ok_or_else(|| invalid(format!("missing desired model for {table}")))?;
        if !model.table.columns.iter().any(|c| c.name == *new_name)
            || model.table.columns.iter().any(|c| c.name == *old_name)
        {
            return Err(invalid(format!(
                "expected {new_name} to replace {table}.{old_name} in desired schema"
            )));
        }
        explicit_changes.push(StateChange::RenameColumn {
            key: key.clone(),
            table: table.clone(),
            old_name: old_name.clone(),
            new_name: new_name.clone(),
        });
    }
    for change in explicit_changes {
        let (owner, ops, after) = next_step(&state, &[change], layout, catalog, origins)?;
        operations.extend(ops.into_iter().map(|op| (owner.clone(), op)));
        state = after;
    }
    #[cfg(feature = "sqlite")]
    let mut checked_removals = BTreeSet::new();
    loop {
        // Re-diff after each transition: a rename updates inbound FKs in
        // state_forwards, so old AlterColumn snapshots are no longer valid.
        let mut changes = state.diff_for_planner(desired)?;
        if changes.is_empty() {
            break;
        }
        if emitting {
            catalog.check_intent(&changes)?;
        }
        #[cfg(feature = "sqlite")]
        for change in &changes {
            if let StateChange::RemoveColumn { table, column, .. } = change
                && checked_removals.insert((table.clone(), column.name.clone()))
            {
                validate_final_column_references(desired, table, &column.name)?;
            }
        }
        order_column_checks(&state, desired, &mut changes)?;
        let (owner, ops, after) = match next_step(&state, &changes, layout, catalog, origins) {
            Ok(step) => step,
            Err(error) if !emitting => {
                let Some(staged) = defer_new_foreign_keys(&state, desired) else {
                    return Err(error);
                };
                // Only new table creations can make progress by deferring FKs.
                let creates: Vec<_> = state
                    .diff_for_planner(&staged)?
                    .into_iter()
                    .filter(|change| matches!(change, StateChange::CreateModel(_)))
                    .collect();
                next_step(&state, &creates, layout, catalog, origins).map_err(|_| error)?
            }
            Err(error) => return Err(error),
        };
        origins.observe(&ops);
        operations.extend(ops.into_iter().map(|op| (owner.clone(), op)));
        state = after;
    }
    Ok(())
}

// Check both ends of a transition: release constraints before dropping an
// input, and install a new constraint after all its inputs are available.
fn order_column_checks(
    state: &ProjectState,
    desired: &ProjectState,
    changes: &mut Vec<StateChange>,
) -> Result<(), MigrationError> {
    let mut blocked_removals = BTreeSet::new();
    let mut release = BTreeMap::new();
    for change in changes.iter() {
        let StateChange::RemoveColumn { key, table, column } = change else {
            continue;
        };
        let current = &state.tables[table_index(state, table)?];
        let final_table = &desired.tables[table_index(desired, table)?];
        let reads_input = |c: &ColumnState| {
            c.check
                .as_deref()
                .is_some_and(|expr| sql_dependencies::mentions_identifier(expr, &column.name))
        };
        for sibling in current
            .columns
            .iter()
            .filter(|c| !same_name(&c.name, &column.name) && reads_input(c))
        {
            blocked_removals.insert((table.clone(), column.name.clone()));
            match final_table
                .columns
                .iter()
                .find(|c| same_name(&c.name, &sibling.name))
            {
                Some(c) if reads_input(c) => {
                    return Err(MigrationError::DependentColumn {
                        table: table.clone(),
                        column: column.name.clone(),
                        dependent: format!("CHECK on {}", sibling.name),
                    });
                }
                Some(_) => {} // The pending AlterColumn will release this input.
                None => {
                    // Both fields are being removed, possibly with mutual CHECKs.
                    let mut new_column = sibling.clone();
                    new_column.check = None;
                    release.insert(
                        (table.clone(), sibling.name.clone()),
                        StateChange::AlterColumn {
                            key: key.clone(),
                            table: table.clone(),
                            old_column: sibling.clone(),
                            new_column,
                        },
                    );
                }
            }
        }
    }
    changes.retain(|change| match change {
        StateChange::RemoveColumn { table, column, .. } => {
            !blocked_removals.contains(&(table.clone(), column.name.clone()))
        }
        _ => true,
    });
    changes.extend(release.into_values());

    let mut pending: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for change in changes.iter() {
        if let StateChange::AddColumn { table, column, .. } = change {
            pending
                .entry(table.clone())
                .or_default()
                .push(column.name.clone());
        }
    }
    changes.retain_mut(|change| {
        let (table, column, adding) = match change {
            StateChange::AddColumn { table, column, .. } => (table, column, true),
            StateChange::AlterColumn {
                table, new_column, ..
            } => (table, new_column, false),
            _ => return true,
        };
        let waits = column.check.as_ref().is_some_and(|expr| {
            pending.get(table).is_some_and(|columns| {
                columns.iter().any(|name| {
                    *name != column.name && sql_dependencies::mentions_identifier(expr, name)
                })
            })
        });
        if waits && adding {
            column.check = None;
        }
        !waits || adding
    });
    Ok(())
}

fn independent_added_columns(left: &MigrationOperation, right: &MigrationOperation) -> bool {
    matches!((left, right), (MigrationOperation::AddColumn { table: a, column: x }, MigrationOperation::AddColumn { table: b, column: y })
        if a == b && !same_name(&x.name, &y.name) && [x, y].iter().all(|c| c.references.is_none() && c.check.is_none() && c.default.is_none() && c.nullable && !c.primary_key))
}

fn invalid(message: impl Into<String>) -> MigrationError {
    MigrationError::InvalidState(message.into())
}

fn validate_file(
    before: &ProjectState,
    ops: &[MigrationOperation],
    catalog: &Catalog<'_>,
) -> Result<ProjectState, MigrationError> {
    #[cfg(feature = "postgres")]
    let state = PostgresSchemaEditor::plan_with_external_targets(
        ops,
        before,
        &catalog.database_checked(before, ops),
    )?
    .resulting_state;
    #[cfg(feature = "sqlite")]
    let state = ops.iter().try_fold(before.clone(), |s, op| {
        let next = SqliteSchemaEditor::plan(op, &s)?.resulting_state;
        catalog::validate_sqlite_namespace(&next)?;
        Ok::<_, MigrationError>(next)
    })?;
    state.validate()?;
    catalog.validate_requirements(&state)?;
    Ok(state)
}

fn write_constraints_changed(old: Option<&ColumnState>, new: Option<&ColumnState>) -> bool {
    old.zip(new).is_some_and(|(old, new)| {
        old.nullable != new.nullable
            || old.primary_key != new.primary_key
            || old.unique != new.unique
            || old.check != new.check
            || old.choices != new.choices
            || old.references != new.references
    })
}

fn same_default(old: Option<&ColumnState>, new: Option<&ColumnState>) -> bool {
    fn shape(column: Option<&ColumnState>) -> (Option<&str>, bool) {
        let explicit = column.and_then(|c| c.default.as_deref());
        let managed = explicit.is_none() && column.is_some_and(|c| c.auto_now || c.auto_now_add);
        (explicit, managed)
    }
    shape(old) == shape(new)
}

fn columns(change: &StateChange) -> &[ColumnState] {
    match change {
        StateChange::CreateModel(model) => &model.table.columns,
        StateChange::AddColumn { column, .. }
        | StateChange::AlterColumn {
            new_column: column, ..
        } => std::slice::from_ref(column),
        _ => &[],
    }
}

fn targets(columns: &[ColumnState]) -> impl Iterator<Item = (&str, &str)> {
    columns
        .iter()
        .filter_map(|c| c.references.as_ref())
        .filter_map(ForeignKeyState::target_parts)
}

fn missing_targets<'a>(
    state: &ProjectState,
    catalog: &Catalog<'_>,
    changes: &[&'a StateChange],
) -> Vec<(&'a str, &'a str)> {
    changes
        .iter()
        .flat_map(|c| targets(columns(c)))
        .filter(|&(table, col)| {
            let Some(key) = catalog.desired_model(table) else {
                return !catalog.available(state, table, col);
            };
            let supplied = changes.iter().any(|change| match change {
                StateChange::CreateModel(m) => &m.key == key && has_key(&m.table, col),
                _ => false,
            });
            let available =
                state.model_keys.get(table) == Some(key) && catalog.available(state, table, col);
            !(supplied || available)
        })
        .collect()
}

type Step = (String, Vec<MigrationOperation>, ProjectState);

fn table_owner<'a>(state: &'a ProjectState, name: &str) -> Option<&'a str> {
    state
        .model_keys
        .get(name)
        .map(|key| key.app.as_str())
        .or_else(|| {
            state
                .objects()
                .iter()
                .find(|object| object.kind == SchemaObjectKind::Table && object.name == name)
                .map(|object| object.app.as_str())
        })
}

// A foreign app can retire a trigger or view in the same release that rebuilds
// its source table. Plan an independently executable rebuild while the old
// consumer still exists. The consumer's owner can then remove it and its data
// in a later file, without making that file a prerequisite for this target.
fn rebuild_before_foreign_removal(
    state: &ProjectState,
    changes: &[StateChange],
    catalog: &Catalog<'_>,
) -> Result<Option<Step>, MigrationError> {
    let retiring: BTreeSet<_> = changes
        .iter()
        .filter_map(|change| {
            let StateChange::DeleteSchemaObject(object) = change else {
                return None;
            };
            (object.kind != SchemaObjectKind::Table
                && !catalog
                    .desired
                    .objects()
                    .iter()
                    .any(|o| o.kind == object.kind && o.name == object.name))
            .then_some((object.app.as_str(), object.kind, object.name.as_str()))
        })
        .collect();
    if retiring.is_empty() {
        return Ok(None);
    }
    for change in changes {
        let Some((key, table)) = model_table(change) else {
            continue;
        };
        if state.model_keys.get(table) != Some(key) {
            continue;
        }
        let op = change.clone().into_operation();
        if SqliteSchemaEditor::rebuild_table_name(&op).is_none() {
            continue;
        }
        let mut projected = state.clone();
        if op.state_forwards(&mut projected).is_err() {
            continue; // Another pending change must run first.
        }
        let batch = state.diff(&projected)?;
        if !batch.iter().any(|change| {
            matches!(change,
                StateChange::DeleteSchemaObject(o) if o.app != key.app
                    && retiring.contains(&(o.app.as_str(), o.kind, o.name.as_str()))
            )
        }) {
            continue;
        }
        if let StateChange::RemoveColumn { column, .. } = change
            && validate_final_column_references(&projected, table, &column.name).is_err()
        {
            continue; // The historical consumer really must be removed first.
        }
        let ops: Vec<_> = state
            .diff_for_planner(&projected)?
            .into_iter()
            .map(StateChange::into_operation)
            .collect();
        if let Ok(after) = validate_file(state, &ops, catalog) {
            return Ok(Some((key.app.clone(), ops, after)));
        }
    }
    Ok(None)
}

fn next_step(
    state: &ProjectState,
    changes: &[StateChange],
    layout: MigrationLayout<'_>,
    catalog: &Catalog<'_>,
    origins: &SqliteIndexOrigins,
) -> Result<Step, MigrationError> {
    if matches!(layout, MigrationLayout::PerApp)
        && let Some(step) = rebuild_before_foreign_removal(state, changes, catalog)?
    {
        return Ok(step);
    }
    let mut blocked = Vec::new();
    for change in changes {
        let dropping_table = match change {
            StateChange::DeleteModel(model) => Some(&model.table.name),
            StateChange::DeleteSchemaObject(object) if object.kind == SchemaObjectKind::Table => {
                Some(&object.name)
            }
            _ => None,
        };
        if let Some(table) = dropping_table
            && let Some(child) = state.objects().iter().find(|o| {
                o.kind == SchemaObjectKind::Table
                    && o.name != *table
                    && sql_dependencies::references_foreign_key(&o.sql, table)
            })
        {
            blocked.push(format!("{} requires table {table}", child.name));
            continue;
        }
        // Operations after a rename use the desired SQL name. A different
        // model can still occupy that name in the current state.
        if let Some((key, table)) = model_table(change)
            && state.model_keys.get(table) != Some(key)
        {
            blocked.push(format!("{}:{} needs table {table}", key.app, key.name));
            continue;
        }
        // Opaque schema objects bracket model transitions (as in diff).
        if matches!(change, StateChange::CreateSchemaObject(o) if o.kind != SchemaObjectKind::Table)
            && changes.iter().any(|c| {
                !matches!(
                    c,
                    StateChange::CreateSchemaObject(_) | StateChange::SetSchemaRequirements { .. }
                )
            })
        {
            continue;
        }
        if let StateChange::CreateSchemaObject(object) = change
            && changes.iter().any(|other| {
                matches!(other, StateChange::CreateSchemaObject(provider)
                if catalog.desired.requires_object(object, provider, false))
            })
        {
            blocked.push(format!(
                "{} needs another pending schema object",
                object.name
            ));
            continue;
        }
        let owner = layout.file_owner(change.app());
        let mut group = vec![change];
        loop {
            let missing = missing_targets(state, catalog, &group);
            let Some(&(table, col)) = missing.first() else {
                break;
            };
            // PostgreSQL can defer a target created in the same file. Follow
            // the closure, including reciprocal FKs, but never mix owners.
            if let Some(target) = changes.iter().find(|c| matches!(c, StateChange::CreateModel(m) if m.table.name == table && layout.file_owner(c.app()) == owner))
                && !group.contains(&target) {
                group.push(target);
            } else {
                blocked.push(format!("{} requires {table}({col})", describe(change)));
                break;
            }
        }
        if !missing_targets(state, catalog, &group).is_empty() {
            continue;
        }
        // Put providers first when possible, retaining a single file for a cycle.
        let mut ops = Vec::new();
        for change in group.into_iter().rev() {
            let op = change.clone().into_operation();
            if let StateChange::RenameTable {
                old_name, new_name, ..
            } = change
            {
                let from = origins.0.get(old_name).unwrap_or(old_name);
                let indexes = &state.tables[table_index(state, old_name)?].indexes;
                if !indexes.is_empty() && from != new_name {
                    // A table and index cannot share a SQLite name, even briefly.
                    // Release the target *before* ALTER TABLE if an index owns it.
                    let before_rename =
                        (0..indexes.len()).any(|i| index_name(from, i) == *new_name);
                    let indexes = MigrationOperation::RenameSqliteIndexes {
                        table: if before_rename { old_name } else { new_name }.clone(),
                        from_table: from.clone(),
                        to_table: new_name.clone(),
                    };
                    if before_rename {
                        ops.extend([indexes, op]);
                    } else {
                        ops.extend([op, indexes]);
                    }
                    continue;
                }
            }
            ops.push(op);
            if let StateChange::CreateSchemaObject(o) = change
                && let Some(requirements) = catalog.desired.object_requirements(o)
            {
                ops.push(MigrationOperation::SetSchemaRequirements {
                    kind: o.kind,
                    name: o.name.clone(),
                    requirements: Some(requirements.clone()),
                });
            }
        }
        match validate_file(state, &ops, catalog) {
            Ok(after) => return Ok((owner.to_owned(), ops, after)),
            Err(error) => blocked.push(format!("{}: {error}", describe(change))),
        }
    }
    Err(invalid(format!(
        "no executable migration step; {}",
        blocked.join("; ")
    )))
}

fn model_table(change: &StateChange) -> Option<(&ModelKey, &str)> {
    match change {
        StateChange::RenameColumn { key, table, .. }
        | StateChange::AddColumn { key, table, .. }
        | StateChange::AlterColumn { key, table, .. }
        | StateChange::RemoveColumn { key, table, .. }
        | StateChange::AddIndex { key, table, .. }
        | StateChange::RemoveIndex { key, table, .. }
        | StateChange::AddUniqueConstraint { key, table, .. }
        | StateChange::RemoveUniqueConstraint { key, table, .. } => Some((key, table)),
        _ => None,
    }
}

fn describe(change: &StateChange) -> String {
    match change {
        StateChange::CreateModel(m) | StateChange::DeleteModel(m) => {
            format!("{}:{}", m.key.app, m.table.name)
        }
        StateChange::AddColumn { table, column, .. }
        | StateChange::AlterColumn {
            table,
            new_column: column,
            ..
        } => format!("{}:{table}.{}", change.app(), column.name),
        _ => format!("{}:{change:?}", change.app()),
    }
}
