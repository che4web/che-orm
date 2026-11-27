//! Schema-version dependencies and their projection onto migration files.
use super::*;

pub(super) struct OperationPlan {
    operations: Vec<(String, MigrationOperation)>,
    // Prerequisites are fixed before any new migration receives a file or ID.
    historical: Vec<BTreeSet<MigrationId>>,
    requires: Vec<BTreeSet<usize>>,
    replacements: Vec<(usize, usize)>,
    borrowed: BTreeSet<usize>,
    emitted: usize,
}

struct PlannedFile {
    owner: String,
    indices: Vec<usize>,
    aliases: BTreeSet<String>,
    consumers: BTreeSet<String>,
}

#[derive(Clone, Copy)]
enum Provider<'a> {
    Frozen(&'a MigrationId),
    Pending(usize),
}

// A schema definition replaces its predecessor. Backfills, however, may fill
// disjoint rows on independent branches, so a data read needs every contributor
// in the current column lifetime. File projection discards transitive edges.
#[derive(Default)]
struct Versions<'a> {
    providers: BTreeMap<SchemaRequirement, Vec<Provider<'a>>>,
}

impl<'a> Versions<'a> {
    fn publish(&mut self, access: &Access, provider: Provider<'a>) {
        for reset in &access.data_resets {
            self.providers.remove(reset);
        }
        for resource in &access.changes {
            let providers = self.providers.entry(resource.clone()).or_default();
            if !matches!(resource, SchemaRequirement::Data(..)) {
                providers.clear();
            }
            providers.push(provider);
        }
    }
}

impl OperationPlan {
    pub(super) fn new(
        graph: &MigrationGraph,
        operations: Vec<(String, MigrationOperation)>,
        catalog: &Catalog<'_>,
        emitted: usize,
    ) -> Result<Self, MigrationError> {
        let mut cursor = ProjectState::default();
        let mut frozen = Vec::new();
        for migration in graph.ordered() {
            frozen.push((
                &migration.id,
                access(&migration.operations, &mut cursor, catalog)?,
            ));
        }
        let mut changes = Vec::new();
        let mut forward_requirements = Vec::new();
        let mut view_appends = Vec::new();
        let mut dropped = BTreeMap::new();
        let mut replacements = Vec::new();
        let mut borrowed = BTreeSet::new();
        for (i, (_, op)) in operations.iter().enumerate() {
            if i == emitted {
                dropped.clear();
            }
            match op {
                MigrationOperation::CreateModel { model } => {
                    // next_step can validate several new models together when
                    // their foreign keys are reciprocal. Keep the forward edge
                    // so an outside consumer also gets the whole cycle.
                    for (target, _) in targets(&model.table.columns) {
                        if target == model.table.name || table_owner(&cursor, target).is_some() {
                            continue;
                        }
                        let mut later = operations.iter().enumerate().skip(i + 1);
                        if let Some((provider, _)) = later.find(|(_, (_, op))| {
                            matches!(op, MigrationOperation::CreateModel { model } if model.table.name == target)
                        }) {
                            forward_requirements.push((i, provider));
                        }
                    }
                }
                MigrationOperation::DropSchemaObject { kind, name }
                    if *kind != SchemaObjectKind::Table =>
                {
                    if let Some(old) = cursor
                        .objects()
                        .iter()
                        .find(|o| o.kind == *kind && o.name == *name)
                    {
                        dropped.insert((*kind, name.as_str()), (i, old.clone()));
                    }
                }
                MigrationOperation::CreateSchemaObject { object } => {
                    if let Some((start, old)) = dropped.remove(&(object.kind, object.name.as_str()))
                    {
                        replacements.push((start, i));
                        if old == *object {
                            borrowed.extend([start, i]);
                        } else if let Some(columns) =
                            sql_dependencies::appended_view_columns(&old, object)
                        {
                            view_appends.push((start, i, object.name.clone(), columns));
                        }
                    }
                }
                _ => {}
            }
            let contract = match (op, operations.get(i + 1)) {
                (
                    MigrationOperation::CreateSchemaObject { object },
                    Some((
                        _,
                        MigrationOperation::SetSchemaRequirements {
                            kind,
                            name,
                            requirements,
                        },
                    )),
                ) if i + 1 != emitted && object.kind == *kind && object.name == *name => {
                    forward_requirements.push((i, i + 1));
                    requirements.as_ref()
                }
                _ => None,
            };
            changes.push(operation_access(
                std::slice::from_ref(op),
                &mut cursor,
                catalog,
                contract,
            )?);
        }
        for &i in &borrowed {
            // A temporarily suspended definition retains its logical version.
            changes[i].changes.clear();
        }
        for (drop, create, view, columns) in view_appends {
            let relation = SchemaRequirement::Relation(physical_key(&view));
            changes[drop].changes.remove(&relation);
            changes[create].changes.remove(&relation);
            Access::publish_view_append(&mut changes[create].changes, &view, columns);
        }
        for (i, change) in changes.iter().enumerate() {
            let prior: Vec<_> = if i < emitted {
                frozen
                    .iter()
                    .map(|(_, access)| access)
                    .chain(&changes[..i])
                    .collect()
            } else {
                changes[..emitted].iter().collect()
            };
            for prior in prior {
                if let Some(resource) = change
                    .uncertain
                    .intersection(&prior.removed)
                    .chain(prior.uncertain.intersection(&change.removed))
                    .next()
                {
                    return Err(MigrationError::NeedsManualMigration {
                        resource: operations[i].1.summary(),
                        reason: format!(
                            "cannot resolve SQL bindings for {resource:?} across a column removal"
                        ),
                    });
                }
            }
        }
        let mut versions = Versions::default();
        for (id, access) in &frozen {
            versions.publish(access, Provider::Frozen(id));
        }
        let mut historical = Vec::new();
        let mut requires = Vec::new();
        for (i, change) in changes.iter().enumerate() {
            let mut files: BTreeSet<_> = frozen
                .iter()
                .filter(|(_, prior)| change.requires_transition(prior))
                .map(|(id, _)| (*id).clone())
                .collect();
            let mut pending: BTreeSet<_> = (0..i)
                .filter(|&j| change.requires_transition(&changes[j]))
                .collect();
            for input in change.needs.union(&change.view_shapes) {
                for provider in versions.providers.get(input).into_iter().flatten() {
                    match provider {
                        Provider::Frozen(id) => {
                            files.insert((*id).clone());
                        }
                        Provider::Pending(j) => {
                            pending.insert(*j);
                        }
                    }
                }
            }
            historical.push(files);
            requires.push(pending);
            versions.publish(change, Provider::Pending(i));
        }
        for (consumer, provider) in forward_requirements {
            requires[consumer].insert(provider);
        }
        // A declared replacement and the physical work that needs it suspended
        // must stay atomic. These internal edges don't version the table.
        for &(start, end) in &replacements {
            for j in start + 1..end {
                if matches!(
                    operations[j].1,
                    MigrationOperation::AddIndex { .. }
                        | MigrationOperation::RemoveIndex { .. }
                        | MigrationOperation::AddUniqueConstraint { .. }
                        | MigrationOperation::RemoveUniqueConstraint { .. }
                        | MigrationOperation::AddColumn { .. }
                        | MigrationOperation::RemoveColumn { .. }
                        | MigrationOperation::AlterColumn { .. }
                ) {
                    // Logical version edges already order edits to columns the
                    // definition actually uses. SQLite handles physical rebuilds.
                    continue;
                }
                if !changes[start].reads.is_disjoint(&changes[j].writes) {
                    requires[j].insert(start);
                }
                if !changes[end].reads.is_disjoint(&changes[j].writes) {
                    requires[end].insert(j);
                }
            }
        }
        Ok(Self {
            operations,
            historical,
            requires,
            replacements,
            borrowed,
            emitted,
        })
    }

    pub(super) fn migrations(
        &self,
        original: &MigrationGraph,
        historical: ProjectState,
        name: &str,
        catalog: &Catalog<'_>,
    ) -> Result<Vec<Migration>, MigrationError> {
        let mut history: Vec<_> = original.ordered().cloned().collect();
        let mut before = historical;
        let mut generated = Vec::new();
        let mut locations: BTreeMap<usize, MigrationId> = BTreeMap::new();
        let mut consumers: BTreeMap<MigrationId, BTreeSet<String>> = BTreeMap::new();
        let mut anchors: BTreeMap<String, BTreeSet<MigrationId>> = BTreeMap::new();
        let groups = self.atomic_groups()?;
        let mut requirements: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
        for group in &groups {
            for app in &group.consumers {
                requirements
                    .entry(app.clone())
                    .or_default()
                    .extend(&group.indices);
            }
        }
        for file in pack_files(groups) {
            let graph = MigrationGraph::new(history.clone())?;
            let mut dependencies = BTreeSet::new();
            for &i in &file.indices {
                dependencies.extend(self.historical[i].iter().cloned());
                for j in &self.requires[i] {
                    if let Some(id) = locations.get(j) {
                        dependencies.insert(id.clone());
                    } else if !file.indices.contains(j) {
                        return Err(invalid("file precedes a required operation"));
                    }
                }
            }
            let mut implied = BTreeSet::new();
            for id in &dependencies {
                let mut closure = graph.target_closure(id)?;
                closure.remove(id);
                implied.extend(closure);
            }
            dependencies.retain(|id| !implied.contains(id));

            // Continue compatible branches of this app. Chaining to a file
            // needed by fewer targets would enlarge a consumer's closure.
            let compatible: Vec<_> = graph
                .ordered()
                .filter(|m| {
                    m.id.app == file.owner
                        && consumers.get(&m.id).map_or_else(
                            || {
                                file.consumers.len() == 1
                                    || dependencies.contains(&m.id)
                                    || implied.contains(&m.id)
                            },
                            |targets| targets.is_superset(&file.consumers),
                        )
                })
                .collect();
            let parents: BTreeSet<_> = compatible.iter().flat_map(|m| &m.dependencies).collect();
            dependencies.extend(
                compatible
                    .iter()
                    .filter(|m| !parents.contains(&m.id))
                    .map(|m| m.id.clone()),
            );
            let migration = Migration::new(
                graph.next_id(&file.owner, name)?,
                dependencies.into_iter().collect(),
                file.indices
                    .iter()
                    .map(|&i| self.operations[i].1.clone())
                    .collect(),
            );
            let mut ancestors = BTreeSet::new();
            for id in &migration.dependencies {
                ancestors.extend(graph.target_closure(id)?);
            }
            validate_file(
                &graph.replay_selected(ProjectState::default(), &ancestors)?,
                &migration.operations,
                catalog,
            )?;
            before = validate_file(&before, &migration.operations, catalog)?;
            for i in file.indices {
                locations.insert(i, migration.id.clone());
            }
            consumers.insert(migration.id.clone(), file.consumers);
            anchors.entry(file.owner).or_default();
            for alias in file.aliases {
                anchors
                    .entry(alias)
                    .or_default()
                    .insert(migration.id.clone());
            }
            history.push(migration.clone());
            generated.push(migration);
        }
        // Incomparable consumer closures may require sibling files. Publish
        // one final target for the owner without making its consumers depend
        // on the join. Atomic changes shared with another app use the same rule.
        for (app, mut required) in anchors {
            let graph = MigrationGraph::new(history.clone())?;
            let heads = graph.app_heads(&app);
            for head in &heads {
                let covered = graph.target_closure(&head.id)?;
                required.retain(|id| !covered.contains(id));
            }
            if required.is_empty() && heads.len() == 1 {
                continue;
            }
            required.extend(heads.into_iter().map(|m| m.id.clone()));
            let target = Migration::new(
                graph.next_id(&app, name)?,
                required.into_iter().collect(),
                vec![],
            );
            history.push(target.clone());
            generated.push(target);
        }
        let graph = MigrationGraph::new(history)?;
        self.validate_target_closures(original, &graph, &requirements, &locations)?;
        original.validate_generated_history(&graph, catalog.desired, catalog)?;
        Ok(generated)
    }

    /// Check the public file graph against the operation graph, including
    /// indirect consumers whose own files will be generated in a later run.
    fn validate_target_closures(
        &self,
        original: &MigrationGraph,
        graph: &MigrationGraph,
        requirements: &BTreeMap<String, BTreeSet<usize>>,
        locations: &BTreeMap<usize, MigrationId>,
    ) -> Result<(), MigrationError> {
        for (app, required) in requirements {
            let mut expected_history = BTreeSet::new();
            for id in original
                .app_heads(app)
                .iter()
                .map(|m| &m.id)
                .chain(required.iter().flat_map(|&i| &self.historical[i]))
            {
                expected_history.extend(original.target_closure(id)?);
            }
            let mut actual = BTreeSet::new();
            for head in graph.app_heads(app) {
                actual.extend(graph.target_closure(&head.id)?);
            }
            // For a future consumer, project its incoming edges onto the
            // provider files emitted in this run. A published target must
            // already reach every required operation through its own head.
            if self.operations[self.emitted..]
                .iter()
                .any(|(owner, _)| owner == app)
            {
                for i in required {
                    actual.extend(graph.target_closure(&locations[i])?);
                }
            }
            for (i, file) in locations {
                if actual.contains(file) != required.contains(i) {
                    return Err(invalid(format!(
                        "file {file:?} changes the operation closure for {app}: {}",
                        self.operations[*i].1.summary()
                    )));
                }
            }
            let actual_history: BTreeSet<_> = actual
                .into_iter()
                .filter(|id| original.migrations.contains_key(id))
                .collect();
            if actual_history != expected_history {
                return Err(invalid(format!(
                    "generated files change the historical prerequisites for {app}: {:?}",
                    actual_history
                        .symmetric_difference(&expected_history)
                        .collect::<Vec<_>>()
                )));
            }
        }
        Ok(())
    }

    fn atomic_groups(&self) -> Result<Vec<PlannedFile>, MigrationError> {
        let mut batches: BTreeMap<_, _> =
            (0..self.operations.len()).map(|i| (i, vec![i])).collect();
        for &(start, end) in &self.replacements {
            batches.remove(&end);
            batches
                .get_mut(&start)
                .expect("replacement start")
                .push(end);
        }
        let mut ordered = Vec::new();
        while !batches.is_empty() {
            let owners: BTreeMap<_, _> = batches
                .iter()
                .flat_map(|(&batch, ops)| ops.iter().map(move |&op| (op, batch)))
                .collect();
            let dependencies: BTreeMap<_, BTreeSet<_>> = batches
                .iter()
                .map(|(&batch, ops)| {
                    let deps = ops
                        .iter()
                        .flat_map(|&op| &self.requires[op])
                        .filter_map(|op| owners.get(op).copied())
                        .filter(|&b| b != batch)
                        .collect();
                    (batch, deps)
                })
                .collect();
            let Some((&ready, _)) = dependencies.iter().find(|(_, deps)| deps.is_empty()) else {
                let cycle = replacement_cycle(&dependencies);
                let first = *cycle
                    .first()
                    .ok_or_else(|| invalid("cannot order atomic replacements"))?;
                let mut merged = Vec::new();
                for batch in cycle {
                    merged.extend(batches.remove(&batch).expect("present cycle member"));
                }
                merged.sort_unstable();
                batches.insert(first, merged);
                continue;
            };
            let indices = batches.remove(&ready).expect("ready batch");
            let owner = indices
                .iter()
                .find_map(|&i| {
                    let (app, op) = &self.operations[i];
                    (!matches!(
                        op,
                        MigrationOperation::DropSchemaObject { .. }
                            | MigrationOperation::CreateSchemaObject { .. }
                    ))
                    .then_some(app)
                })
                .unwrap_or(&self.operations[indices[0]].0)
                .clone();
            let aliases: BTreeSet<_> = indices
                .iter()
                .filter(|&&i| self.operations[i].0 != owner && !self.borrowed.contains(&i))
                .map(|&i| self.operations[i].0.clone())
                .collect();
            let mut consumers = aliases.clone();
            consumers.insert(owner.clone());
            ordered.push(PlannedFile {
                owner,
                indices,
                aliases,
                consumers,
            });
        }
        // Propagate application targets backwards through the operation DAG.
        // Two atomic groups may share a file only when every target that
        // needs one already needs the other.
        let mut group_of = vec![0; self.operations.len()];
        for (g, file) in ordered.iter().enumerate() {
            for &i in &file.indices {
                group_of[i] = g;
            }
        }
        for g in (0..ordered.len()).rev() {
            let (prior, rest) = ordered.split_at_mut(g);
            let file = &rest[0];
            for dep in file.indices.iter().flat_map(|&i| &self.requires[i]) {
                let provider = group_of[*dep];
                if provider != g {
                    prior[provider]
                        .consumers
                        .extend(file.consumers.iter().cloned());
                }
            }
        }
        // Dependencies have a superset of their consumer's targets. The
        // stable sort keeps topological order for equally shared groups.
        ordered.retain(|group| group.indices[0] < self.emitted);
        if ordered
            .iter()
            .flat_map(|g| &g.indices)
            .any(|&i| i >= self.emitted)
        {
            return Err(invalid(
                "scoped migration requires an atomic change in another application",
            ));
        }
        Ok(ordered)
    }
}

fn pack_files(mut ordered: Vec<PlannedFile>) -> Vec<PlannedFile> {
    ordered.sort_by_key(|f| std::cmp::Reverse(f.consumers.len()));
    let mut files: Vec<PlannedFile> = Vec::new();
    for file in ordered {
        if let Some(previous) = files.last_mut()
            && previous.owner == file.owner
            && previous.consumers == file.consumers
            && previous.aliases.is_empty()
            && file.aliases.is_empty()
        {
            previous.indices.extend(file.indices);
        } else {
            files.push(file);
        }
    }
    files
}

fn replacement_cycle(edges: &BTreeMap<usize, BTreeSet<usize>>) -> BTreeSet<usize> {
    let reachable = |start, graph: &BTreeMap<usize, BTreeSet<usize>>| {
        let mut seen = BTreeSet::new();
        let mut todo = vec![start];
        while let Some(node) = todo.pop() {
            if seen.insert(node) {
                todo.extend(&graph[&node]);
            }
        }
        seen
    };
    let mut reversed: BTreeMap<_, BTreeSet<_>> =
        edges.keys().map(|&node| (node, BTreeSet::new())).collect();
    for (&node, deps) in edges {
        for dep in deps {
            reversed.get_mut(dep).expect("known batch").insert(node);
        }
    }
    for &node in edges.keys() {
        let forward = reachable(node, edges);
        let backward = reachable(node, &reversed);
        let cycle: BTreeSet<_> = forward.intersection(&backward).copied().collect();
        if cycle.len() > 1 {
            return cycle;
        }
    }
    BTreeSet::new()
}

// An FK removal releases a key, rather than reading the whole parent.
// Preserve its order against changes to that key, without pulling destructive
// child migrations into unrelated changes such as adding a parent index.
// None represents any key on the named table.
type ForeignKeyResource = (String, Option<String>);

fn foreign_key_resource(table: &str, column: &str) -> ForeignKeyResource {
    (table.into(), Some(physical_key(column)))
}

// Each resource has a new version only when its definition changes. In
// particular an index does not publish a new version of its table's columns.

#[derive(Default)]
pub(super) struct Access {
    reads: BTreeSet<String>,
    writes: BTreeSet<String>,
    exclusive_writes: BTreeSet<String>,
    trigger_writes: BTreeSet<String>,
    data_resets: BTreeSet<SchemaRequirement>,
    column_writes: BTreeSet<(String, String)>,
    appended_columns: BTreeSet<String>,
    needs: BTreeSet<SchemaRequirement>,
    view_shapes: BTreeSet<SchemaRequirement>,
    changes: BTreeSet<SchemaRequirement>,
    // Undetermined SQL bindings cannot justify a destructive prerequisite.
    uncertain: BTreeSet<SchemaRequirement>,
    removed: BTreeSet<SchemaRequirement>,
    released: BTreeSet<SchemaRequirement>,
    released_foreign_keys: BTreeSet<ForeignKeyResource>,
    changed_keys: BTreeSet<ForeignKeyResource>,
    opaque: bool,
}
impl Access {
    // A later incarnation of this name cannot inherit earlier row values.
    fn reset_column_data(&mut self, table: &str, column: &str) {
        let data = SchemaRequirement::Data(table.into(), physical_key(column));
        self.changes.remove(&data);
        self.data_resets.insert(data);
    }

    pub(super) fn requires_table(&self, name: &str) -> bool {
        self.opaque
            || self
                .needs
                .contains(&SchemaRequirement::Relation(physical_key(name)))
            || self.writes.contains(name)
    }

    pub(super) fn check_scoped_object(
        object: &SchemaObjectState,
        selected: &ProjectState,
        desired: &ProjectState,
        policy: &GenerationPolicy,
    ) -> Result<(), MigrationError> {
        let mut inputs = Access::default();
        inputs.record_object(
            object,
            desired,
            policy.sql_tables.get(&object.name),
            desired.object_requirements(object),
        );
        let mut versions = Access::default();
        for provider in desired
            .models()
            .into_iter()
            .filter(|m| m.key.app != object.app)
        {
            versions.writes.insert(provider.table.name);
        }
        versions.record_versions(selected, desired);
        for input in inputs.needs.union(&inputs.view_shapes) {
            if !versions.changes.contains(input) {
                continue;
            }
            let (_, table, column) = input.parts();
            let mut name = table.to_owned();
            if let Some(column) = column {
                name = format!("{table}.{column}");
            } else if matches!(input, SchemaRequirement::ColumnList(_))
                && let Some(field) =
                    desired
                        .tables()
                        .iter()
                        .find(|t| t.name == table)
                        .and_then(|t| {
                            t.columns.iter().find(|c| {
                                !selected.tables().iter().any(|old| {
                                    old.name == table
                                        && old.columns.iter().any(|f| same_name(&f.name, &c.name))
                                })
                            })
                        })
            {
                name = format!("{table}.{}", field.name);
            }
            return Err(invalid(format!(
                "schema object {} requires {name}; generate and register the provider migration first",
                object.name
            )));
        }
        Ok(())
    }

    fn require_key(&mut self, table: &str, column: &str) {
        self.reads.insert(table.into());
        self.needs.insert(SchemaRequirement::Relation(table.into()));
        self.needs
            .insert(SchemaRequirement::Key(table.into(), physical_key(column)));
    }

    fn record_object(
        &mut self,
        object: &SchemaObjectState,
        state: &ProjectState,
        sql_table: Option<&SqlTableContract>,
        declaration: Option<&BTreeSet<SchemaRequirement>>,
    ) {
        // A consumer of v.id only inherits id from stars expanded by v.
        // The migration creating v already supplies its fixed inputs.
        let names: BTreeSet<_> = state
            .tables()
            .iter()
            .flat_map(|t| t.columns.iter().map(|c| c.name.as_str()))
            .collect();
        let view_columns: BTreeMap<_, _> = state
            .objects()
            .iter()
            .filter(|o| o.kind == SchemaObjectKind::View)
            .map(|o| (o.name.as_str(), sql_dependencies::direct_view_columns(o)))
            .collect();
        let copies_rows = object.kind == SchemaObjectKind::Table
            && sql_dependencies::SqlReads::new(object).copies_rows();
        let mut sources = vec![(object, None::<BTreeSet<&str>>, copies_rows)];
        let mut visited = BTreeSet::new();
        while let Some((source, projection, reads_data)) = sources.pop() {
            if !visited.insert((
                source.kind,
                source.name.as_str(),
                projection.clone(),
                reads_data,
            )) {
                continue;
            }
            let declared = if source == object {
                declaration
            } else {
                state.object_requirements(source)
            };
            if let Some(inputs) = declared {
                let reads = sql_dependencies::SqlReads::new(source);
                for input in inputs {
                    let (_, table, _) = input.parts();
                    self.reads.insert(table.into());
                    if !matches!(input, SchemaRequirement::Index(_)) {
                        self.needs.insert(SchemaRequirement::Relation(table.into()));
                    }
                    let fields = match input {
                        SchemaRequirement::ColumnList(_) => {
                            if let Some(model) = state.tables().iter().find(|t| t.name == table) {
                                for c in &model.columns {
                                    if projection
                                        .as_ref()
                                        .is_some_and(|p| !p.iter().any(|n| same_name(n, &c.name)))
                                        && reads.column(table, &c.name, state) == Some(false)
                                    {
                                        continue;
                                    }
                                    if projection.is_some() {
                                        self.needs.insert(SchemaRequirement::Column(
                                            table.into(),
                                            physical_key(&c.name),
                                        ));
                                    }
                                    if reads_data {
                                        self.needs.insert(SchemaRequirement::Data(
                                            table.into(),
                                            physical_key(&c.name),
                                        ));
                                    }
                                }
                            }
                            if projection.is_none() {
                                if object.kind == SchemaObjectKind::View {
                                    self.view_shapes.insert(input.clone());
                                } else {
                                    self.needs.insert(input.clone());
                                }
                            }
                            projection.clone()
                        }
                        SchemaRequirement::Data(_, name) => {
                            self.needs
                                .insert(SchemaRequirement::Column(table.into(), name.clone()));
                            self.needs.insert(input.clone());
                            Some(BTreeSet::from([name.as_str()]))
                        }
                        SchemaRequirement::Default(_, name)
                        | SchemaRequirement::WriteConstraint(_, name) => {
                            self.needs
                                .insert(SchemaRequirement::Column(table.into(), name.clone()));
                            self.needs.insert(input.clone());
                            continue;
                        }
                        SchemaRequirement::Column(_, name) => {
                            if reads_data && source != object {
                                self.needs
                                    .insert(SchemaRequirement::Data(table.into(), name.clone()));
                            }
                            self.needs.insert(input.clone());
                            Some(BTreeSet::from([name.as_str()]))
                        }
                        _ => {
                            self.needs.insert(input.clone());
                            continue;
                        }
                    };
                    if let Some(view) = state
                        .objects()
                        .iter()
                        .find(|o| o.kind == SchemaObjectKind::View && same_name(&o.name, table))
                    {
                        if let Some(fields) = &fields {
                            self.needs.extend(fields.iter().map(|name| {
                                SchemaRequirement::Column(
                                    physical_key(&view.name),
                                    physical_key(name),
                                )
                            }));
                        }
                        sources.push((
                            view,
                            fields,
                            reads_data || matches!(input, SchemaRequirement::Data(..)),
                        ));
                    }
                }
                continue;
            }
            let reads = sql_dependencies::SqlReads::new(source);
            for table in state.tables() {
                if !reads.references(&table.name) {
                    continue;
                }
                self.reads.insert(table.name.clone());
                self.needs
                    .insert(SchemaRequirement::Relation(table.name.clone()));
                let projected_inputs = projection
                    .as_ref()
                    .and_then(|fields| reads.projected_inputs(&table.name, fields));
                let shape = SchemaRequirement::ColumnList(table.name.clone());
                let uses_shape = reads.column_list(&table.name);
                if uses_shape.is_none() {
                    self.uncertain.insert(shape.clone());
                }
                if projection.is_none() && uses_shape.unwrap_or(true) {
                    if object.kind == SchemaObjectKind::View {
                        self.view_shapes.insert(shape);
                    } else {
                        self.needs.insert(shape);
                    }
                }
                for (ordinal, column) in table.columns.iter().enumerate() {
                    if reads.checks_write(&table.name, &column.name) {
                        self.needs.insert(SchemaRequirement::WriteConstraint(
                            table.name.clone(),
                            physical_key(&column.name),
                        ));
                    }
                    let resource =
                        SchemaRequirement::Column(table.name.clone(), physical_key(&column.name));
                    let uses_column = if let Some(inputs) = &projected_inputs {
                        Some(inputs.contains(&physical_key(&column.name)))
                    } else {
                        reads.column(&table.name, &column.name, state)
                    };
                    if uses_column.is_none() {
                        self.uncertain.insert(resource.clone());
                    }
                    if uses_column.unwrap_or(true)
                        || uses_shape.unwrap_or(true)
                            && projection
                                .as_ref()
                                .is_some_and(|fields| fields.contains(column.name.as_str()))
                    {
                        self.needs.insert(resource);
                    }
                    if reads_data
                        && (uses_column.unwrap_or(true)
                            || uses_shape.unwrap_or(true)
                                && projection
                                    .as_ref()
                                    .is_none_or(|fields| fields.contains(column.name.as_str())))
                    {
                        self.needs.insert(SchemaRequirement::Data(
                            table.name.clone(),
                            physical_key(&column.name),
                        ));
                    }
                    if reads.uses_default(&table.name, &column.name, ordinal) {
                        self.needs.insert(SchemaRequirement::Default(
                            table.name.clone(),
                            physical_key(&column.name),
                        ));
                    }
                }
                for i in 0..table.indexes.len() {
                    let index = index_name(&table.name, i);
                    if sql_dependencies::mentions_identifier(&source.sql, &index) {
                        self.needs.insert(SchemaRequirement::Index(index));
                    }
                }
            }
            for provider in state
                .objects()
                .iter()
                .filter(|o| requires_schema_object(source, o, false))
            {
                self.reads.insert(provider.name.clone());
                self.needs
                    .insert(SchemaRequirement::Relation(physical_key(&provider.name)));
                if provider.kind == SchemaObjectKind::View {
                    let output_names = &view_columns[provider.name.as_str()];
                    let star = reads.column_list(&provider.name).unwrap_or(true)
                        || reads.reads_whole_row(&provider.name);
                    let projected_inputs = projection
                        .as_ref()
                        .and_then(|fields| reads.projected_inputs(&provider.name, fields));
                    let fields: Option<BTreeSet<_>> = if star && projection.is_none() {
                        None
                    } else {
                        Some(
                            names
                                .iter()
                                .copied()
                                .chain(output_names.iter().map(String::as_str))
                                .filter(|name| {
                                    if let Some(inputs) = &projected_inputs {
                                        return inputs.contains(&physical_key(name));
                                    }
                                    reads.column(&provider.name, name, state) == Some(true)
                                        || star
                                            && projection.as_ref().is_some_and(|p| p.contains(name))
                                })
                                .collect(),
                        )
                    };
                    if let Some(fields) = &fields {
                        self.needs.extend(fields.iter().map(|column| {
                            SchemaRequirement::Column(
                                physical_key(&provider.name),
                                physical_key(column),
                            )
                        }));
                    } else {
                        self.view_shapes
                            .insert(SchemaRequirement::ColumnList(physical_key(&provider.name)));
                    }
                    sources.push((provider, fields, reads_data));
                }
            }
        }
        if let Some(contract) = sql_table.filter(|_| object.kind == SchemaObjectKind::Table) {
            for (table, key) in &contract.requires {
                self.require_key(table, key);
            }
        }
    }

    fn publish_view_append(
        changes: &mut BTreeSet<SchemaRequirement>,
        name: &str,
        columns: Vec<String>,
    ) {
        changes.insert(SchemaRequirement::ColumnList(physical_key(name)));
        changes.extend(
            columns
                .into_iter()
                .map(|column| SchemaRequirement::Column(physical_key(name), column)),
        );
    }

    // Compare the two boundaries of an indivisible historical file (or one
    // pending operation). A temporary drop/recreate within frozen history
    // doesn't change the version supplied to a new consumer.
    fn record_versions(&mut self, before: &ProjectState, after: &ProjectState) {
        for name in &self.writes {
            let old = before.tables().iter().find(|t| t.name == *name);
            let new = after.tables().iter().find(|t| t.name == *name);
            let relation = |o: &&SchemaObjectState| {
                o.kind != SchemaObjectKind::Trigger && same_name(&o.name, name)
            };
            let previous = before.objects().iter().find(relation);
            let current = after.objects().iter().find(relation);
            if let Some(columns) = previous
                .zip(current)
                .and_then(|(old, new)| sql_dependencies::appended_view_columns(old, new))
            {
                Self::publish_view_append(&mut self.changes, name, columns);
            } else if old.is_some() != new.is_some() || previous != current {
                self.changes
                    .insert(SchemaRequirement::Relation(physical_key(name)));
            }
            if old == new {
                continue;
            }
            let old_columns = old.map_or(&[][..], |t| t.columns.as_slice());
            let new_columns = new.map_or(&[][..], |t| t.columns.as_slice());
            let projection = |c: &ColumnState| (physical_key(&c.name), c.column_type);
            if old_columns
                .iter()
                .map(projection)
                .ne(new_columns.iter().map(projection))
            {
                self.changes
                    .insert(SchemaRequirement::ColumnList(name.clone()));
            }
            for col in old_columns.iter().chain(new_columns) {
                let old_col = old_columns.iter().find(|c| same_name(&c.name, &col.name));
                let new_col = new_columns.iter().find(|c| same_name(&c.name, &col.name));
                if old_col.is_some() && new_col.is_none() {
                    self.removed
                        .insert(SchemaRequirement::ColumnList(name.clone()));
                    self.removed.insert(SchemaRequirement::Column(
                        name.clone(),
                        physical_key(&col.name),
                    ));
                }
                if write_constraints_changed(old_col, new_col) {
                    self.changes.insert(SchemaRequirement::WriteConstraint(
                        name.clone(),
                        physical_key(&col.name),
                    ));
                }
                let old_check = old_col.and_then(|c| c.check.as_deref());
                let new_check = new_col.and_then(|c| c.check.as_deref());
                if old_check != new_check {
                    for input in old_columns.iter().chain(new_columns) {
                        if old_check
                            .into_iter()
                            .chain(new_check)
                            .any(|sql| sql_dependencies::mentions_identifier(sql, &input.name))
                        {
                            self.changes.insert(SchemaRequirement::WriteConstraint(
                                name.clone(),
                                physical_key(&input.name),
                            ));
                        }
                    }
                }
                if !same_default(old_col, new_col) {
                    self.changes.insert(SchemaRequirement::Default(
                        name.clone(),
                        physical_key(&col.name),
                    ));
                }
                if old_col.map(projection) != new_col.map(projection) {
                    self.changes.insert(SchemaRequirement::Column(
                        name.clone(),
                        physical_key(&col.name),
                    ));
                }
                if old.is_some_and(|t| has_key(t, &col.name))
                    != new.is_some_and(|t| has_key(t, &col.name))
                    || old_col.map(|c| c.column_type) != new_col.map(|c| c.column_type)
                {
                    self.changes.insert(SchemaRequirement::Key(
                        name.clone(),
                        physical_key(&col.name),
                    ));
                }
            }
            let old_unique: BTreeSet<_> = old
                .into_iter()
                .flat_map(|t| &t.unique_constraints)
                .collect();
            let new_unique: BTreeSet<_> = new
                .into_iter()
                .flat_map(|t| &t.unique_constraints)
                .collect();
            for group in old_unique.symmetric_difference(&new_unique) {
                for column in group.iter() {
                    self.changes.insert(SchemaRequirement::WriteConstraint(
                        name.clone(),
                        physical_key(column),
                    ));
                }
            }
            let old_indexes = old.map_or(&[][..], |t| t.indexes.as_slice());
            let new_indexes = new.map_or(&[][..], |t| t.indexes.as_slice());
            for i in 0..old_indexes.len().max(new_indexes.len()) {
                if old_indexes.get(i) != new_indexes.get(i) {
                    self.changes
                        .insert(SchemaRequirement::Index(index_name(name, i)));
                }
            }
        }
    }

    fn requires_transition(&self, prior: &Self) -> bool {
        !self.trigger_writes.is_disjoint(&prior.trigger_writes)
            || !self.exclusive_writes.is_disjoint(&prior.writes)
            || !self.writes.is_disjoint(&prior.exclusive_writes)
            || !self.column_writes.is_disjoint(&prior.column_writes)
            || !self.appended_columns.is_disjoint(&prior.appended_columns)
            // The editor refreshes already-installed wildcard views. Installing a
            // writer may require a default or constraint; changing one does not
            // require installing or retiring a writer.
            || self.changes.iter().any(|r| !matches!(r, SchemaRequirement::Default(..) | SchemaRequirement::WriteConstraint(..))
                && (prior.released.contains(r) || prior.needs.contains(r)))
            || self.releases_keys_changed_by(prior)
            || prior.releases_keys_changed_by(self)
    }

    fn releases_keys_changed_by(&self, other: &Self) -> bool {
        self.released_foreign_keys.iter().any(|(table, column)| {
            other
                .changed_keys
                .iter()
                .any(|(changed_table, changed_column)| {
                    table == changed_table
                        && (column.is_none()
                            || changed_column.is_none()
                            || column == changed_column)
                })
        })
    }

    pub(super) fn conflicts(&self, other: &Self) -> bool {
        self.opaque
            || other.opaque
            || !self.trigger_writes.is_disjoint(&other.trigger_writes)
            || !self.writes.is_disjoint(&other.reads)
            || !self.reads.is_disjoint(&other.writes)
            || !self.writes.is_disjoint(&other.writes)
            || self.releases_keys_changed_by(other)
            || other.releases_keys_changed_by(self)
    }
}

pub(super) fn access(
    ops: &[MigrationOperation],
    state: &mut ProjectState,
    catalog: &Catalog<'_>,
) -> Result<Access, MigrationError> {
    operation_access(ops, state, catalog, None)
}

fn operation_access(
    ops: &[MigrationOperation],
    state: &mut ProjectState,
    catalog: &Catalog<'_>,
    creation_requirements: Option<&BTreeSet<SchemaRequirement>>,
) -> Result<Access, MigrationError> {
    use MigrationOperation::*;
    let mut a = Access::default();
    let before = state.clone();
    let mut remaining: Vec<_> = ops.iter().rev().collect();
    while let Some(op) = remaining.pop() {
        let mut writes = BTreeSet::new();
        let table = match op {
            CreateModel {
                model: ModelState { table, .. },
            }
            | CreateTable { table } => {
                for (target, column) in targets(&table.columns) {
                    a.require_key(target, column);
                }
                &table.name
            }
            RenameTable { old_name, new_name } => {
                writes.insert(old_name.clone());
                a.changed_keys.insert((old_name.clone(), None));
                // A table rename implicitly rewrites all existing inbound FKs.
                for t in state.tables() {
                    if targets(&t.columns).any(|(target, _)| target == old_name) {
                        writes.insert(t.name.clone());
                    }
                }
                new_name
            }
            AddColumn { table, column } => {
                for (target, key) in targets(std::slice::from_ref(column)) {
                    a.require_key(target, key);
                }
                table
            }
            AlterColumn {
                table,
                old_column,
                new_column,
            } => {
                let old_target = targets(std::slice::from_ref(old_column)).next();
                let new_target = targets(std::slice::from_ref(new_column)).next();
                if old_target != new_target
                    && let Some((target, column)) = old_target
                {
                    a.released_foreign_keys
                        .insert(foreign_key_resource(target, column));
                }
                if let Some((target, column)) = new_target {
                    a.require_key(target, column);
                }
                if old_column.column_type != new_column.column_type
                    || old_column.primary_key != new_column.primary_key
                    || old_column.unique != new_column.unique
                {
                    a.changed_keys
                        .insert(foreign_key_resource(table, &old_column.name));
                }
                table
            }
            DropTable { name } => {
                let old = &state.tables[table_index(state, name)?];
                for column in &old.columns {
                    a.reset_column_data(name, &column.name);
                }
                a.released_foreign_keys.extend(
                    targets(&old.columns)
                        .map(|(target, column)| foreign_key_resource(target, column)),
                );
                a.changed_keys.insert((name.clone(), None));
                name
            }
            RemoveColumn { table, column } => {
                a.reset_column_data(table, column);
                let old = &state.tables[table_index(state, table)?];
                let old_column = &old.columns[column_index(old, column)?];
                a.released_foreign_keys.extend(
                    targets(std::slice::from_ref(old_column))
                        .map(|(target, column)| foreign_key_resource(target, column)),
                );
                a.changed_keys.insert(foreign_key_resource(table, column));
                table
            }
            RemoveUniqueConstraint { table, columns } => {
                if let [column] = columns.as_slice() {
                    a.changed_keys.insert(foreign_key_resource(table, column));
                }
                table
            }
            RenameColumn {
                table, old_name, ..
            } => {
                a.changed_keys.insert(foreign_key_resource(table, old_name));
                for t in state.tables() {
                    if targets(&t.columns)
                        .any(|(target, col)| target == table && same_name(col, old_name))
                    {
                        writes.insert(t.name.clone());
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
                    for index in [index_name(from_table, i), index_name(to_table, i)] {
                        writes.insert(index.clone());
                        a.changes.insert(SchemaRequirement::Index(index));
                    }
                }
                table
            }
            RenameModel { table, .. }
            | AddIndex { table, .. }
            | RemoveIndex { table, .. }
            | AddUniqueConstraint { table, .. }
            | NormalizeSqliteDateTimes { table, .. } => table,
            SetSchemaRequirements {
                kind,
                name,
                requirements,
            } => {
                let object = state
                    .objects()
                    .iter()
                    .find(|o| o.kind == *kind && o.name == *name)
                    .ok_or_else(|| invalid(format!("missing {kind:?} {name}")))?;
                a.record_object(object, state, catalog.contract(name), requirements.as_ref());
                if *kind != SchemaObjectKind::Trigger {
                    a.needs
                        .insert(SchemaRequirement::Relation(physical_key(name)));
                }
                name
            }
            CreateSchemaObject { object } => {
                let requirements = creation_requirements.or_else(|| match remaining.last() {
                    Some(SetSchemaRequirements {
                        kind,
                        name,
                        requirements,
                    }) if object.kind == *kind && object.name == *name => requirements.as_ref(),
                    _ => None,
                });
                a.record_object(object, state, catalog.contract(&object.name), requirements);
                &object.name
            }
            DropSchemaObject { kind, name } => {
                let object = state
                    .objects()
                    .iter()
                    .find(|object| object.kind == *kind && object.name == *name)
                    .ok_or_else(|| invalid(format!("missing {kind:?} {name}")))?;
                if *kind == SchemaObjectKind::Table {
                    a.changed_keys.insert((name.clone(), None));
                    if let Some(contract) = catalog.contract(name) {
                        a.released_foreign_keys.extend(
                            contract
                                .requires
                                .iter()
                                .map(|(target, column)| foreign_key_resource(target, column)),
                        );
                    } else {
                        // Legacy SQL tables can omit a contract. A named REFERENCES
                        // target still needs ordering against changes to any of its keys.
                        for source in [&*state, catalog.desired] {
                            a.released_foreign_keys.extend(
                                source
                                    .tables()
                                    .iter()
                                    .map(|table| &table.name)
                                    .chain(
                                        source
                                            .objects()
                                            .iter()
                                            .filter(|o| o.kind == SchemaObjectKind::Table)
                                            .map(|o| &o.name),
                                    )
                                    .filter(|target| {
                                        sql_dependencies::references_foreign_key(
                                            &object.sql,
                                            target,
                                        )
                                    })
                                    .map(|target| (target.clone(), None)),
                            );
                        }
                    }
                } else {
                    let mut old = Access::default();
                    old.record_object(
                        object,
                        state,
                        catalog.contract(&object.name),
                        state.object_requirements(object),
                    );
                    a.reads.extend(old.reads);
                    a.released.extend(old.needs);
                }
                name
            }
            BackfillColumn { table, column, .. } => {
                // The expression can read other rows (a merge conflict), but a
                // data write does not publish a new version of any schema.
                a.changes
                    .insert(SchemaRequirement::Data(table.clone(), physical_key(column)));
                a.opaque = true;
                a.reads.insert(table.clone());
                a.needs.insert(SchemaRequirement::Relation(table.clone()));
                a.needs.insert(SchemaRequirement::Column(
                    table.clone(),
                    physical_key(column),
                ));
                op.state_forwards(state)?;
                continue;
            }
            RunSql {
                state_operations, ..
            } => {
                // Explicit dependencies order opaque data work. Publish schema
                // versions only for the changes declared by the author.
                a.opaque = true;
                remaining.extend(state_operations.iter().rev());
                continue;
            }
        };
        if matches!(
            op,
            CreateSchemaObject {
                object: SchemaObjectState {
                    kind: SchemaObjectKind::Trigger,
                    ..
                }
            } | DropSchemaObject {
                kind: SchemaObjectKind::Trigger,
                ..
            } | SetSchemaRequirements {
                kind: SchemaObjectKind::Trigger,
                ..
            }
        ) {
            a.trigger_writes.insert(physical_key(table));
            op.state_forwards(state)?;
            continue;
        }
        // The operation that releases an index name must precede a file that
        // reuses it, including when only that file's ancestors are deployed.
        if let DropTable { name } | RemoveIndex { table: name, .. } = op {
            let before = &state.tables[table_index(state, name)?];
            writes.extend((0..before.indexes.len()).map(|i| index_name(name, i)));
        }
        writes.insert(table.clone());
        let mut index_change = false;
        let fields = match op {
            AddColumn { column, .. } => {
                // Appends to the same relation preserve positional SQL semantics.
                a.appended_columns.insert(table.clone());
                Some(std::slice::from_ref(&column.name))
            }
            RemoveColumn { column, .. } => Some(std::slice::from_ref(column)),
            AlterColumn { new_column, .. } => Some(std::slice::from_ref(&new_column.name)),
            AddIndex { columns, .. } | RemoveIndex { columns, .. } => {
                index_change = true;
                Some(columns.as_slice())
            }
            AddUniqueConstraint { columns, .. } | RemoveUniqueConstraint { columns, .. } => {
                Some(columns.as_slice())
            }
            _ => None,
        };
        if let Some(fields) = fields {
            a.column_writes
                .extend(fields.iter().map(|c| (table.clone(), physical_key(c))));
            // CHECKs on a changed column can also read sibling columns.
            // An unrelated CHECK elsewhere on the table is not a dependency.
            let checks: Vec<_> = match op {
                AddColumn { column, .. } => vec![column],
                AlterColumn {
                    old_column,
                    new_column,
                    ..
                } => vec![old_column, new_column],
                RemoveColumn { column, .. } => {
                    let schema = &state.tables[table_index(state, table)?];
                    vec![&schema.columns[column_index(schema, column)?]]
                }
                _ => vec![],
            };
            for expr in checks.into_iter().filter_map(|c| c.check.as_deref()) {
                for c in &state.tables[table_index(state, table)?].columns {
                    if sql_dependencies::mentions_identifier(expr, &c.name) {
                        a.needs.insert(SchemaRequirement::Column(
                            table.clone(),
                            physical_key(&c.name),
                        ));
                    }
                }
            }
        } else {
            a.exclusive_writes.insert(table.clone());
        }
        op.state_forwards(state)?;
        if (fields.is_none() || index_change)
            && let Some(schema) = state.tables().iter().find(|t| t.name == *table)
        {
            writes.extend((0..schema.indexes.len()).map(|i| index_name(table, i)));
        }
        a.exclusive_writes
            .extend(writes.iter().filter(|w| *w != table).cloned());
        a.writes.extend(writes);
    }
    a.record_versions(&before, state);
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_validation_rejects_added_pending_and_frozen_dependencies() {
        let file_id = |app: &str, name: &str| MigrationId {
            app: app.into(),
            name: name.into(),
        };
        let view = |app: &str| MigrationOperation::CreateSchemaObject {
            object: SchemaObjectState {
                app: app.into(),
                kind: SchemaObjectKind::View,
                name: format!("{app}_view"),
                sql: format!("CREATE VIEW {app}_view AS SELECT 1 AS id"),
            },
        };
        let frozen = Migration::new(file_id("stock", "0001_old"), vec![], vec![]);
        let original = MigrationGraph::new(vec![frozen.clone()]).unwrap();
        let stock = Migration::new(
            file_id("stock", "0002_new"),
            vec![frozen.id.clone()],
            vec![view("stock")],
        );
        let history = ProjectState::default();
        let policy = GenerationPolicy::default();
        let catalog = Catalog::new(&history, &history, &policy).unwrap();
        let plan = OperationPlan::new(
            &original,
            vec![
                ("stock".into(), view("stock")),
                ("reports".into(), view("reports")),
            ],
            &catalog,
            2,
        )
        .unwrap();
        let requirements = BTreeMap::from([("reports".to_owned(), BTreeSet::from([1]))]);
        for (extra, diagnostic) in [
            (&stock.id, "operation closure"),
            (&frozen.id, "historical prerequisites"),
        ] {
            let report = Migration::new(
                file_id("reports", "0001_report"),
                vec![extra.clone()],
                vec![view("reports")],
            );
            let locations = BTreeMap::from([(0, stock.id.clone()), (1, report.id.clone())]);
            let graph = MigrationGraph::new(vec![frozen.clone(), stock.clone(), report]).unwrap();
            let error = plan
                .validate_target_closures(&original, &graph, &requirements, &locations)
                .unwrap_err();
            assert!(error.to_string().contains(diagnostic), "{error}");
        }
    }
}
