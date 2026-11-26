//! Offline planning for newly generated migrations. Historical operations and
//! checksums are inputs, never regenerated from the current model definitions.
use super::catalog::{Catalog, has_key, same_name};
use super::*;

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
            for op in operations {
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
            return Err(error);
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
        let Ok(mut files) = self.generate(&staged, layout, label) else {
            return Err(error);
        };
        let base = MigrationGraph::new(
            self.ordered()
                .cloned()
                .chain(files.iter().cloned())
                .collect(),
        )?;
        files.extend(base.generate(desired, layout, "relationships")?);
        Ok(files)
    }

    /// Plans a single application's changes against the compiled history of
    /// other apps. Historical dependent objects can be temporarily removed
    /// and restored in the same file when a table needs rebuilding.
    pub fn generate_for_app(
        &self,
        desired: &ProjectState,
        app: &str,
        name: &str,
        policy: &GenerationPolicy,
    ) -> Result<Vec<Migration>, MigrationError> {
        let mut selected = self.replay(ProjectState::default())?;
        selected.tables.retain(|table| {
            selected
                .model_keys
                .get(&table.name)
                .is_none_or(|key| key.app != app)
        });
        selected.model_keys.retain(|_, key| key.app != app);
        selected.objects.retain(|object| object.app != app);
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
            for table in desired.tables().iter().map(|table| &table.name).chain(
                desired
                    .objects()
                    .iter()
                    .filter(|provider| provider.kind == SchemaObjectKind::Table)
                    .map(|provider| &provider.name),
            ) {
                if sql_dependencies::references(object, table)
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
            for provider in desired
                .objects()
                .iter()
                .filter(|provider| provider.kind == SchemaObjectKind::View && provider.app != app)
            {
                if references_view(object, provider)
                    && !selected.objects().iter().any(|candidate| {
                        candidate.kind == SchemaObjectKind::View
                            && same_name(&candidate.name, &provider.name)
                    })
                {
                    return Err(invalid(format!(
                        "schema object {} requires view {}; generate and register its migration first",
                        object.name, provider.name
                    )));
                }
            }
            selected.add_schema_object(object.clone())?;
        }
        self.generate_with_policy(&selected, MigrationLayout::Single(app), name, policy)
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
        self.validate_heads()?;
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(invalid("invalid generated migration name"));
        }
        let historical = self.replay(ProjectState::default())?;
        let catalog = Catalog::new(&historical, desired, policy)?;
        let mut state = historical.clone();
        let mut groups: Vec<(String, Vec<MigrationOperation>)> = Vec::new();
        // Frozen RenameTable files kept the old physical SQLite index names.
        // Reclaim those names before another model needs them. The same logical
        // operations are valid on PostgreSQL and preserve both histories.
        let mut origins = SqliteIndexOrigins::default();
        for migration in self.ordered() {
            origins.observe(&migration.operations);
        }
        for (owner, op) in origins.repairs(&historical, desired)? {
            op.state_forwards(&mut state)?;
            origins.observe(std::slice::from_ref(&op));
            append_group(&mut groups, layout.file_owner(&owner), vec![op]);
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
            append_group(&mut groups, layout.file_owner(app), vec![op]);
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
            let (owner, ops, after) = next_step(&state, &[change], layout, &catalog, &origins)?;
            append_group(&mut groups, &owner, ops);
            state = after;
        }
        #[cfg(feature = "sqlite")]
        let mut checked_removals = BTreeSet::new();
        loop {
            // Re-diff after each transition: a rename updates inbound FKs in
            // state_forwards, so old AlterColumn snapshots are no longer valid.
            let changes = state.diff(desired)?;
            if changes.is_empty() {
                break;
            }
            catalog.check_intent(&changes)?;
            #[cfg(feature = "sqlite")]
            for change in &changes {
                if let StateChange::RemoveColumn { table, column, .. } = change
                    && checked_removals.insert((table.clone(), column.name.clone()))
                {
                    validate_final_column_references(desired, table, &column.name)?;
                }
            }
            let (owner, ops, after) = next_step(&state, &changes, layout, &catalog, &origins)?;
            origins.observe(&ops);
            append_group(&mut groups, &owner, ops);
            state = after;
        }
        if groups.is_empty() {
            return Ok(Vec::new());
        }
        let mut history: Vec<_> = self.ordered().cloned().collect();
        let mut cursor = ProjectState::default();
        let mut accesses = Vec::new();
        for m in &history {
            accesses.push((m.id.clone(), access(&m.operations, &mut cursor, &catalog)?));
        }
        let mut generated = Vec::new();
        let groups = atomic_schema_replacements(groups, &historical);
        let mut before = historical;
        for (app, operations, aliases) in groups {
            let graph = MigrationGraph::new(history.clone())?;
            let touched = access(&operations, &mut cursor, &catalog)?;
            let mut dependencies: BTreeSet<_> = accesses
                .iter()
                .filter(|(_, a)| touched.conflicts(a))
                .map(|(id, _)| id.clone())
                .collect();
            // Drop ancestors already implied by a later dependency.
            let mut implied = BTreeSet::new();
            for id in &dependencies {
                let mut closure = graph.target_closure(id)?;
                closure.remove(id);
                implied.extend(closure);
            }
            dependencies.retain(|id| !implied.contains(id));
            // app_heads follows direct same-app edges, even if a cross-app
            // dependency transitively includes the previous head.
            dependencies.extend(graph.app_heads(&app).iter().map(|m| m.id.clone()));
            let migration = Migration::new(
                graph.next_id(&app, name)?,
                dependencies.into_iter().collect(),
                operations,
            );
            // Also check a targeted deployment with only this file's ancestors.
            let mut ancestors = BTreeSet::new();
            for id in &migration.dependencies {
                ancestors.extend(graph.target_closure(id)?);
            }
            validate_file(
                &graph.replay_selected(ProjectState::default(), &ancestors)?,
                &migration.operations,
                &catalog,
            )?;
            before = validate_file(&before, &migration.operations, &catalog)?;
            accesses.push((migration.id.clone(), touched));
            history.push(migration.clone());
            let anchor = migration.id.clone();
            generated.push(migration);
            for app in aliases {
                let graph = MigrationGraph::new(history.clone())?;
                let mut dependencies: BTreeSet<_> = graph
                    .app_heads(&app)
                    .iter()
                    .map(|head| head.id.clone())
                    .collect();
                dependencies.insert(anchor.clone());
                let alias = Migration::new(
                    graph.next_id(&app, name)?,
                    dependencies.into_iter().collect(),
                    vec![],
                );
                history.push(alias.clone());
                generated.push(alias);
            }
        }
        self.validate_generated_history(history, desired, &catalog)?;
        Ok(generated)
    }

    fn validate_generated_history(
        &self,
        history: Vec<Migration>,
        desired: &ProjectState,
        catalog: &Catalog<'_>,
    ) -> Result<(), MigrationError> {
        let graph = MigrationGraph::new(history)?;
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

// Ask SQLite to resolve a view against the final model columns and registered
// views. Preparing a SELECT resolves nested projections without reading rows.
// External tables and application functions may only exist at deployment;
// for those, retain the conservative offline dependency check.
#[cfg(feature = "sqlite")]
fn view_catalog(desired: &ProjectState) -> Result<crate::rusqlite::Connection, MigrationError> {
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
        // Registered SQL tables provide columns to views just like model tables.
        for table in desired
            .objects()
            .iter()
            .filter(|o| o.kind == SchemaObjectKind::Table)
        {
            db.execute(&table.sql, [])?;
        }
        for view in desired
            .objects()
            .iter()
            .filter(|o| o.kind == SchemaObjectKind::View)
        {
            db.execute(&view.sql, [])?;
        }
        Ok(db)
    };
    open().map_err(|e| invalid(format!("cannot prepare final view catalog: {e}")))
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
                    None => catalog.insert(view_catalog(desired)?),
                };
                let select = format!("SELECT * FROM {} LIMIT 0", quote_identifier(&object.name));
                match db.prepare(&select) {
                    Ok(_) => continue,
                    Err(
                        crate::rusqlite::Error::SqliteFailure(_, Some(message))
                        | crate::rusqlite::Error::SqlInputError { msg: message, .. },
                    ) if message.starts_with("no such column:")
                        || message.starts_with("ambiguous column name:") =>
                    {
                        true
                    }
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
        order_schema_objects(affected.into_values().collect(), true)?
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

fn independent_added_columns(left: &MigrationOperation, right: &MigrationOperation) -> bool {
    matches!((left, right), (MigrationOperation::AddColumn { table: a, column: x }, MigrationOperation::AddColumn { table: b, column: y })
        if a == b && !same_name(&x.name, &y.name) && [x, y].iter().all(|c| c.references.is_none() && c.check.is_none() && c.default.is_none() && c.nullable && !c.primary_key))
}

fn append_group(
    groups: &mut Vec<(String, Vec<MigrationOperation>)>,
    owner: &str,
    ops: Vec<MigrationOperation>,
) {
    if let Some((last, operations)) = groups.last_mut()
        && last == owner
    {
        operations.extend(ops);
    } else {
        groups.push((owner.to_owned(), ops));
    }
}

// A target may stop at any migration file. Keep the full interval from
// removing a schema object to installing its replacement in the same file.
// If the interval includes work owned by several apps, give each affected app
// a target that depends on the file where the operations actually run.
type AtomicGroup = (String, Vec<MigrationOperation>, BTreeSet<String>);

fn atomic_schema_replacements(
    groups: Vec<(String, Vec<MigrationOperation>)>,
    historical: &ProjectState,
) -> Vec<AtomicGroup> {
    let operations: Vec<_> = groups
        .into_iter()
        .flat_map(|(app, ops)| {
            ops.into_iter()
                .map(move |operation| (app.clone(), operation))
        })
        .collect();
    let historical: BTreeMap<_, _> = historical
        .objects()
        .iter()
        .map(|object| ((object.kind, object.name.as_str()), object))
        .collect();
    let mut removed = BTreeMap::new();
    let mut intervals = BTreeMap::new();
    let mut temporary = BTreeSet::new();
    for (index, (_, operation)) in operations.iter().enumerate() {
        match operation {
            MigrationOperation::DropSchemaObject { kind, name }
                if *kind != SchemaObjectKind::Table =>
            {
                removed.insert((*kind, name.as_str()), index);
            }
            MigrationOperation::CreateSchemaObject { object } => {
                let key = (object.kind, object.name.as_str());
                if let Some(start) = removed.remove(&key) {
                    intervals.insert(start, index);
                    // An unchanged object is borrowed for the model change.
                    // The owning application needs no new target of its own.
                    if historical.get(&key) == Some(&object) {
                        temporary.insert(start);
                        temporary.insert(index);
                    }
                }
            }
            _ => {}
        }
    }
    let mut result: Vec<AtomicGroup> = Vec::new();
    let mut start = 0;
    while start < operations.len() {
        if let Some(&end) = intervals.get(&start) {
            let mut end = end;
            let mut cursor = start + 1;
            while cursor <= end {
                if let Some(&overlap) = intervals.get(&cursor) {
                    end = end.max(overlap);
                }
                cursor += 1;
            }
            let batch = &operations[start..=end];
            let owner = batch
                .iter()
                .find(|(_, operation)| {
                    !matches!(
                        operation,
                        MigrationOperation::DropSchemaObject { .. }
                            | MigrationOperation::CreateSchemaObject { .. }
                    )
                })
                .unwrap_or(&batch[0])
                .0
                .clone();
            let aliases = batch
                .iter()
                .enumerate()
                .filter(|(offset, (app, _))| {
                    app != &owner && !temporary.contains(&(start + offset))
                })
                .map(|(_, (app, _))| app.clone())
                .collect();
            result.push((
                owner,
                batch
                    .iter()
                    .map(|(_, operation)| operation.clone())
                    .collect(),
                aliases,
            ));
            start = end + 1;
        } else {
            let (app, operation) = &operations[start];
            if let Some((last_app, last_ops, aliases)) = result.last_mut()
                && last_app == app
                && aliases.is_empty()
            {
                last_ops.push(operation.clone());
            } else {
                result.push((app.clone(), vec![operation.clone()], BTreeSet::new()));
            }
            start += 1;
        }
    }
    result
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

fn requires_schema_object(object: &SchemaObjectState, provider: &SchemaObjectState) -> bool {
    references_view(object, provider)
        || (provider.kind == SchemaObjectKind::Table
            && object != provider
            && sql_dependencies::references(object, &provider.name))
}

fn next_step(
    state: &ProjectState,
    changes: &[StateChange],
    layout: MigrationLayout<'_>,
    catalog: &Catalog<'_>,
    origins: &SqliteIndexOrigins,
) -> Result<Step, MigrationError> {
    let mut blocked = Vec::new();
    for change in changes {
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
            && changes
                .iter()
                .any(|c| !matches!(c, StateChange::CreateSchemaObject(_)))
        {
            continue;
        }
        if let StateChange::CreateSchemaObject(object) = change
            && changes.iter().any(|other| {
                matches!(other, StateChange::CreateSchemaObject(provider)
                if requires_schema_object(object, provider))
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

// Table-level read/write hazards across file boundaries. Adding/removing an FK
// reads its target; dropping or renaming that target must wait for those reads.
// For opaque SQL/objects we conservatively impose a history barrier.
#[derive(Default)]
struct Access {
    reads: BTreeSet<String>,
    writes: BTreeSet<String>,
    opaque: bool,
}
impl Access {
    fn conflicts(&self, other: &Self) -> bool {
        self.opaque
            || other.opaque
            || !self.writes.is_disjoint(&other.reads)
            || !self.reads.is_disjoint(&other.writes)
            || !self.writes.is_disjoint(&other.writes)
    }
}

fn access(
    ops: &[MigrationOperation],
    state: &mut ProjectState,
    catalog: &Catalog<'_>,
) -> Result<Access, MigrationError> {
    use MigrationOperation::*;
    let mut a = Access::default();
    for op in ops {
        let table = match op {
            CreateModel {
                model: ModelState { table, .. },
            }
            | CreateTable { table } => {
                a.reads
                    .extend(targets(&table.columns).map(|(t, _)| t.to_owned()));
                &table.name
            }
            RenameTable { old_name, new_name } => {
                a.writes.insert(old_name.clone());
                // A table rename implicitly rewrites all existing inbound FKs.
                for t in state.tables() {
                    if targets(&t.columns).any(|(target, _)| target == old_name) {
                        a.writes.insert(t.name.clone());
                    }
                }
                new_name
            }
            AddColumn { table, column } => {
                a.reads
                    .extend(targets(std::slice::from_ref(column)).map(|(t, _)| t.to_owned()));
                table
            }
            AlterColumn {
                table,
                old_column,
                new_column,
            } => {
                for c in [old_column, new_column] {
                    a.reads
                        .extend(targets(std::slice::from_ref(c)).map(|(t, _)| t.to_owned()));
                }
                table
            }
            DropTable { name } => {
                let old = &state.tables[table_index(state, name)?];
                a.reads
                    .extend(targets(&old.columns).map(|(t, _)| t.to_owned()));
                name
            }
            RemoveColumn { table, column } => {
                let old = &state.tables[table_index(state, table)?];
                a.reads.extend(
                    targets(std::slice::from_ref(
                        &old.columns[column_index(old, column)?],
                    ))
                    .map(|(t, _)| t.to_owned()),
                );
                table
            }
            RenameColumn {
                table, old_name, ..
            } => {
                for t in state.tables() {
                    if targets(&t.columns)
                        .any(|(target, col)| target == table && same_name(col, old_name))
                    {
                        a.writes.insert(t.name.clone());
                    }
                }
                table
            }
            RenameSqliteIndexes {
                table,
                from_table,
                to_table,
            } => {
                let schema = &state.tables[table_index(state, table)?];
                for i in 0..schema.indexes.len() {
                    a.writes.insert(index_name(from_table, i));
                    a.writes.insert(index_name(to_table, i));
                }
                table
            }
            RenameModel { table, .. }
            | AddIndex { table, .. }
            | RemoveIndex { table, .. }
            | AddUniqueConstraint { table, .. }
            | RemoveUniqueConstraint { table, .. }
            | NormalizeSqliteDateTimes { table, .. } => table,
            CreateSchemaObject {
                object:
                    SchemaObjectState {
                        kind: SchemaObjectKind::Table,
                        name,
                        ..
                    },
            }
            | DropSchemaObject {
                kind: SchemaObjectKind::Table,
                name,
            } => {
                if let Some(contract) = catalog.contract(name) {
                    a.reads
                        .extend(contract.requires.iter().map(|(t, _)| t.clone()));
                } else {
                    a.opaque = true;
                }
                name
            }
            BackfillColumn { .. }
            | RunSql { .. }
            | CreateSchemaObject { .. }
            | DropSchemaObject { .. } => {
                a.opaque = true;
                op.state_forwards(state)?;
                continue;
            }
        };
        // The operation that releases an index name must precede a file that
        // reuses it, including when only that file's ancestors are deployed.
        if let DropTable { name } | RemoveIndex { table: name, .. } = op {
            let before = &state.tables[table_index(state, name)?];
            a.writes
                .extend((0..before.indexes.len()).map(|i| index_name(name, i)));
        }
        a.writes.insert(table.clone());
        op.state_forwards(state)?;
        if let Some(schema) = state.tables().iter().find(|t| t.name == *table) {
            for i in 0..schema.indexes.len() {
                a.writes.insert(index_name(table, i));
            }
        }
    }
    Ok(a)
}
