//! Offline planning for newly generated migrations. Historical operations and
//! checksums are inputs, never regenerated from the current model definitions.
use super::catalog::{Catalog, has_key, model_key, physical_key, same_name};
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

    fn reused_index_names(
        &self,
        desired: &ProjectState,
    ) -> Result<BTreeMap<String, BTreeSet<String>>, MigrationError> {
        // Track only bare legacy renames. A rebuild or drop releases every alias.
        let mut aliases: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut state = ProjectState::default();
        for op in self.ordered().flat_map(|file| &file.operations) {
            match op {
                MigrationOperation::RenameTable { old_name, new_name } => {
                    let mut names = aliases.remove(old_name).unwrap_or_default();
                    if !state.tables[table_index(&state, old_name)?]
                        .indexes
                        .is_empty()
                    {
                        names.insert(old_name.clone());
                        aliases.insert(new_name.clone(), names);
                    }
                }
                MigrationOperation::DropTable { name } => {
                    aliases.remove(name);
                }
                _ => {
                    if let Some(table) = SqliteSchemaEditor::rebuild_table_name(op) {
                        aliases.remove(table);
                    }
                }
            }
            op.state_forwards(&mut state)?;
        }
        aliases.retain(|owner, names| {
            names.retain(|name| {
                !same_name(owner, name)
                    && desired
                        .tables()
                        .iter()
                        .any(|table| same_name(&table.name, name) && !table.indexes.is_empty())
            });
            !names.is_empty()
                && state.model_keys.contains_key(owner)
                && state
                    .tables()
                    .iter()
                    .any(|t| &t.name == owner && !t.indexes.is_empty())
        });
        Ok(aliases)
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
        let index_repairs = if historical.diff(desired)?.is_empty() {
            BTreeMap::new()
        } else {
            self.reused_index_names(desired)?
        };
        for table in index_repairs.keys() {
            let old = &historical.tables[table_index(&historical, table)?];
            let columns = old
                .indexes
                .last()
                .expect("repair requires a historical index");
            // Removing and restoring the last index preserves ordinal names and
            // lets the SQLite editor rebuild every historical index on the table.
            let ops = vec![
                MigrationOperation::RemoveIndex {
                    table: table.clone(),
                    columns: columns.clone(),
                },
                MigrationOperation::AddIndex {
                    table: table.clone(),
                    columns: columns.clone(),
                },
            ];
            for op in &ops {
                op.state_forwards(&mut state)?;
            }
            append_group(
                &mut groups,
                layout.file_owner(&historical.model_keys[table].app),
                ops,
            );
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
        for ((table, old_name), new_name) in &policy.rename_columns {
            let key = state
                .model_keys
                .get(table)
                .ok_or_else(|| invalid(format!("unknown historical table {table}")))?;
            let owner = layout.file_owner(&key.app).to_owned();
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
            let op = MigrationOperation::RenameColumn {
                table: table.clone(),
                old_name: old_name.clone(),
                new_name: new_name.clone(),
            };
            op.state_forwards(&mut state)?;
            append_group(&mut groups, &owner, vec![op]);
        }
        loop {
            // Re-diff after each transition: a rename updates inbound FKs in
            // state_forwards, so old AlterColumn snapshots are no longer valid.
            let changes = state.diff(desired)?;
            if changes.is_empty() {
                break;
            }
            catalog.check_intent(&changes)?;
            let (owner, ops, after) = next_step(&state, &changes, layout, &catalog)?;
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
        let mut before = historical;
        for (app, operations) in groups {
            let graph = MigrationGraph::new(history.clone())?;
            let mut touched = access(&operations, &mut cursor, &catalog)?;
            for (table, names) in &index_repairs {
                if touched.writes.contains(&physical_key(table)) {
                    touched
                        .writes
                        .extend(names.iter().map(|name| physical_key(name)));
                }
            }
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
            generated.push(migration);
        }
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
                replayed = validate_file(&replayed, &file.operations, &catalog)?;
            }
        }
        if !replayed.diff(desired)?.is_empty() {
            return Err(invalid("generated history differs from registered schema"));
        }
        Ok(generated)
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
        SqliteSchemaEditor::plan(op, &s).map(|p| p.resulting_state)
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
                model_key(state, table) == Some(key) && catalog.available(state, table, col);
            !(supplied || available)
        })
        .collect()
}

type Step = (String, Vec<MigrationOperation>, ProjectState);

fn next_step(
    state: &ProjectState,
    changes: &[StateChange],
    layout: MigrationLayout<'_>,
    catalog: &Catalog<'_>,
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
        let owner = layout.file_owner(change.app());
        let mut group = vec![change];
        loop {
            let missing = missing_targets(state, catalog, &group);
            let Some(&(table, col)) = missing.first() else {
                break;
            };
            // PostgreSQL can defer a target created in the same file. Follow
            // the closure, including reciprocal FKs, but never mix owners.
            if let Some(target) = changes.iter().find(|c| matches!(c, StateChange::CreateModel(m) if same_name(&m.table.name, table) && layout.file_owner(c.app()) == owner))
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
            // SQLite keeps index names across ALTER TABLE RENAME. Encode the
            // transition in new files so existing migration history stays frozen.
            if let StateChange::RenameTable {
                old_name, new_name, ..
            } = change
            {
                let indexes = &state.tables[table_index(state, old_name)?].indexes;
                ops.extend(
                    indexes
                        .iter()
                        .rev()
                        .map(|columns| MigrationOperation::RemoveIndex {
                            table: old_name.clone(),
                            columns: columns.clone(),
                        }),
                );
                ops.push(change.clone().into_operation());
                ops.extend(indexes.iter().map(|columns| MigrationOperation::AddIndex {
                    table: new_name.clone(),
                    columns: columns.clone(),
                }));
            } else {
                ops.push(change.clone().into_operation());
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
        StateChange::AddColumn { key, table, .. }
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
                    .extend(targets(&table.columns).map(|(t, _)| physical_key(t)));
                &table.name
            }
            RenameTable { old_name, new_name } => {
                a.writes.insert(physical_key(old_name));
                // A table rename implicitly rewrites all existing inbound FKs.
                for t in state.tables() {
                    if targets(&t.columns).any(|(target, _)| same_name(target, old_name)) {
                        a.writes.insert(physical_key(&t.name));
                    }
                }
                new_name
            }
            AddColumn { table, column } => {
                a.reads
                    .extend(targets(std::slice::from_ref(column)).map(|(t, _)| physical_key(t)));
                table
            }
            AlterColumn {
                table,
                old_column,
                new_column,
            } => {
                for c in [old_column, new_column] {
                    a.reads
                        .extend(targets(std::slice::from_ref(c)).map(|(t, _)| physical_key(t)));
                }
                table
            }
            DropTable { name } => {
                let old = &state.tables[table_index(state, name)?];
                a.reads
                    .extend(targets(&old.columns).map(|(t, _)| physical_key(t)));
                name
            }
            RemoveColumn { table, column } => {
                let old = &state.tables[table_index(state, table)?];
                a.reads.extend(
                    targets(std::slice::from_ref(
                        &old.columns[column_index(old, column)?],
                    ))
                    .map(|(t, _)| physical_key(t)),
                );
                table
            }
            RenameColumn {
                table, old_name, ..
            } => {
                for t in state.tables() {
                    if targets(&t.columns)
                        .any(|(target, col)| same_name(target, table) && same_name(col, old_name))
                    {
                        a.writes.insert(physical_key(&t.name));
                    }
                }
                table
            }
            RenameModel { table, .. }
            | AddIndex { table, .. }
            | RemoveIndex { table, .. }
            | AddUniqueConstraint { table, .. }
            | RemoveUniqueConstraint { table, .. }
            | BackfillColumn { table, .. }
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
                        .extend(contract.requires.iter().map(|(t, _)| physical_key(t)));
                } else {
                    a.opaque = true;
                }
                name
            }
            RunSql { .. } | CreateSchemaObject { .. } | DropSchemaObject { .. } => {
                a.opaque = true;
                op.state_forwards(state)?;
                continue;
            }
        };
        a.writes.insert(physical_key(table));
        op.state_forwards(state)?;
    }
    Ok(a)
}
