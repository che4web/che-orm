use che_orm::{MigrationGraph, MigrationLayout, Model, ProjectState, SchemaSet};

#[derive(Debug, Model)]
#[orm(table = "parents")]
struct Parent {
    #[orm(primary_key)]
    id: i64,
}

#[derive(Debug, Model)]
#[orm(table = "children")]
struct Child {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = Parent)]
    parent_id: Option<i64>,
}

#[derive(Debug, Model)]
#[orm(table = "unrelated")]
struct Unrelated {
    #[orm(primary_key)]
    id: i64,
}

#[derive(Debug, Model)]
#[orm(table = "case_children")]
struct CaseChild {
    #[orm(primary_key)]
    id: i64,
    #[orm(references = "raw_parent(ID)")]
    parent_id: i64,
}

#[tokio::test]
async fn fk_provider_lookup_uses_backend_identifier_rules() {
    use che_orm::{GenerationPolicy, SchemaObjectKind, SchemaObjectState, SqlTableContract};
    let mut desired = ProjectState::from_app_schema("a", &SchemaSet::new().model::<CaseChild>());
    desired
        .add_schema_object(SchemaObjectState {
            app: "z".into(),
            kind: SchemaObjectKind::Table,
            name: "raw_parent".into(),
            sql: "CREATE TABLE raw_parent (id INTEGER PRIMARY KEY)".into(),
        })
        .unwrap();
    let g = MigrationGraph::new(vec![]).unwrap();
    let policy = GenerationPolicy {
        strict_external: true,
        sql_tables: [(
            "raw_parent".into(),
            SqlTableContract {
                keys: ["id".into()].into(),
                ..Default::default()
            },
        )]
        .into(),
        ..Default::default()
    };
    let result = g.generate_with_policy(&desired, MigrationLayout::PerApp, "initial", &policy);
    #[cfg(feature = "postgres")]
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("requires raw_parent(ID)")
    );
    #[cfg(feature = "sqlite")]
    {
        let files = result.unwrap();
        assert_eq!(files[0].id.app, "z");
        assert_eq!(
            files[1].dependencies.as_slice(),
            std::slice::from_ref(&files[0].id)
        );
        let child = files[1].id.clone();
        let db = che_orm::Database::connect_in_memory().await.unwrap();
        let graph = MigrationGraph::new(files).unwrap();
        assert_eq!(db.migrate_to(graph.clone(), Some(child)).await.unwrap(), 2);
        assert_eq!(db.migrate(graph).await.unwrap(), 0);
    }
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_namespace_includes_generated_indexes() {
    use che_orm::{MigrationOperation, SchemaObjectKind, SchemaObjectState};
    for (kind, name, sql) in [
        (
            SchemaObjectKind::Table,
            "parents_idx_0",
            "CREATE TABLE parents_idx_0 (id INTEGER PRIMARY KEY)",
        ),
        (
            SchemaObjectKind::View,
            "PARENTS_IDX_0",
            "CREATE VIEW PARENTS_IDX_0 AS SELECT 1 AS id",
        ),
    ] {
        let mut desired =
            ProjectState::from_app_schema("models", &SchemaSet::new().model::<Parent>());
        MigrationOperation::AddIndex {
            table: "parents".into(),
            columns: vec!["id".into()],
        }
        .state_forwards(&mut desired)
        .unwrap();
        desired
            .add_schema_object(SchemaObjectState {
                app: "raw".into(),
                kind,
                name: name.into(),
                sql: sql.into(),
            })
            .unwrap();
        let error = MigrationGraph::new(vec![])
            .unwrap()
            .generate(&desired, MigrationLayout::PerApp, "initial")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("models:parents_idx_0") && error.contains(&format!("raw:{name}")),
            "{error}"
        );
    }
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_reports_both_owners_of_case_colliding_relation_names() {
    use che_orm::{SchemaObjectKind, SchemaObjectState};

    let mut desired = ProjectState::from_app_schema("models", &SchemaSet::new().model::<Parent>());
    desired
        .add_schema_object(SchemaObjectState {
            app: "raw".into(),
            kind: SchemaObjectKind::View,
            name: "PARENTS".into(),
            sql: "CREATE VIEW PARENTS AS SELECT 1 AS id".into(),
        })
        .unwrap();
    let error = MigrationGraph::new(vec![])
        .unwrap()
        .generate(&desired, MigrationLayout::PerApp, "initial")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("models:parents") && error.contains("raw:PARENTS"),
        "{error}"
    );
}

#[test]
fn drop_dependent_before_parent_even_when_parent_sorts_first() {
    let p = SchemaSet::new().model::<Parent>();
    let c = SchemaSet::new().model::<Child>();
    let u = SchemaSet::new().model::<Unrelated>();
    let desired = ProjectState::from_app_schemas([("a", &p), ("b", &u), ("z", &c)]).unwrap();
    let empty = MigrationGraph::new(vec![]).unwrap();
    let initial = empty
        .generate(&desired, MigrationLayout::PerApp, "initial")
        .unwrap();
    let unrelated = initial.iter().find(|m| m.id.app == "b").unwrap();
    assert!(unrelated.dependencies.is_empty());
    assert!(
        initial
            .iter()
            .all(|m| !m.dependencies.contains(&unrelated.id))
    );

    let desired = ProjectState::from_app_schemas([("b", &u)]).unwrap();
    let graph = MigrationGraph::new(initial.clone()).unwrap();
    assert!(matches!(
        graph.generate(&desired, MigrationLayout::PerApp, "drop_pair"),
        Err(che_orm::MigrationError::NeedsManualMigration { .. })
    ));
    let drops = graph
        .generate_with_policy(
            &desired,
            MigrationLayout::PerApp,
            "drop_pair",
            &che_orm::GenerationPolicy {
                allow_drop: ["children".into(), "parents".into()].into(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        drops.iter().map(|m| m.id.app.as_str()).collect::<Vec<_>>(),
        ["z", "a"]
    );
    assert!(drops[1].dependencies.contains(&drops[0].id));
    assert!(
        drops
            .iter()
            .all(|m| !m.dependencies.contains(&unrelated.id))
    );
    let graph = MigrationGraph::new(initial.into_iter().chain(drops).collect()).unwrap();
    assert!(
        graph
            .generate(&desired, MigrationLayout::PerApp, "again")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn sql_contracts_sort_providers_without_linking_independent_apps() {
    use che_orm::{GenerationPolicy, SchemaObjectKind, SchemaObjectState, SqlTableContract};
    let mut state =
        ProjectState::from_app_schema("unrelated", &SchemaSet::new().model::<Unrelated>());
    for (app, name, definition) in [
        (
            "aaa",
            "child_raw",
            "id BIGINT PRIMARY KEY, p BIGINT REFERENCES z_parent(id)",
        ),
        ("zzz", "z_parent", "id BIGINT PRIMARY KEY"),
    ] {
        state
            .add_schema_object(SchemaObjectState {
                app: app.into(),
                kind: SchemaObjectKind::Table,
                name: name.into(),
                sql: format!("CREATE TABLE {name} ({definition})"),
            })
            .unwrap();
    }
    let mut policy = GenerationPolicy {
        strict_external: true,
        ..Default::default()
    };
    policy.sql_tables.insert(
        "z_parent".into(),
        SqlTableContract {
            keys: ["id".into()].into(),
            ..Default::default()
        },
    );
    policy.sql_tables.insert(
        "child_raw".into(),
        SqlTableContract {
            keys: ["id".into()].into(),
            requires: [("z_parent".into(), "id".into())].into(),
        },
    );
    let graph = MigrationGraph::new(vec![]).unwrap();
    let files = graph
        .generate_with_policy(&state, MigrationLayout::PerApp, "initial", &policy)
        .unwrap();
    let parent = files.iter().find(|m| m.id.app == "zzz").unwrap();
    let child = files.iter().find(|m| m.id.app == "aaa").unwrap();
    assert!(parent.dependencies.is_empty());
    assert_eq!(
        child.dependencies.as_slice(),
        std::slice::from_ref(&parent.id)
    );
    policy
        .external_keys
        .insert(("z_parent".into(), "id".into()));
    assert!(
        graph
            .generate_with_policy(&state, MigrationLayout::PerApp, "initial", &policy)
            .unwrap_err()
            .to_string()
            .contains("cannot be external")
    );
    policy.external_keys.clear();
    policy
        .sql_tables
        .get_mut("child_raw")
        .unwrap()
        .requires
        .insert(("unknown".into(), "id".into()));
    assert!(
        graph
            .generate_with_policy(&state, MigrationLayout::PerApp, "initial", &policy)
            .unwrap_err()
            .to_string()
            .contains("unknown FK target")
    );
}

#[derive(Debug, Model)]
#[orm(table = "left_table")]
struct Left {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = Right)]
    right_id: Option<i64>,
}

#[derive(Debug, Model)]
#[orm(table = "right_table")]
struct Right {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = Left)]
    left_id: Option<i64>,
}

#[test]
fn cycle_diagnostic_names_both_targets_and_single_history_can_group_them() {
    let l = SchemaSet::new().model::<Left>();
    let r = SchemaSet::new().model::<Right>();
    let desired = ProjectState::from_app_schemas([("a", &l), ("b", &r)]).unwrap();
    let graph = MigrationGraph::new(vec![]).unwrap();
    let error = graph
        .generate(&desired, MigrationLayout::PerApp, "initial")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("a:left_table requires right_table(id)"),
        "{error}"
    );
    assert!(
        error.contains("b:right_table requires left_table(id)"),
        "{error}"
    );
    let single = graph
        .generate(&desired, MigrationLayout::Single("shared"), "initial")
        .unwrap();
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].operations.len(), 2);
}

#[test]
fn manual_merge_only_joins_compatible_branches() {
    use che_orm::{Migration, MigrationId, MigrationOperation};
    let state = ProjectState::from_app_schema("app", &SchemaSet::new().model::<Parent>());
    let initial = MigrationGraph::new(vec![])
        .unwrap()
        .generate(&state, MigrationLayout::PerApp, "initial")
        .unwrap()
        .remove(0);
    let branch = |name: &str, field: &str| {
        let mut column = state.tables()[0].columns[0].clone();
        column.name = field.into();
        column.field_name = field.into();
        column.primary_key = false;
        column.nullable = true;
        Migration::new(
            MigrationId {
                app: "app".into(),
                name: name.into(),
            },
            vec![initial.id.clone()],
            vec![MigrationOperation::AddColumn {
                table: "parents".into(),
                column,
            }],
        )
    };
    let left = branch("0002_left", "a");
    let right = branch("0002_right", "b");
    let graph = MigrationGraph::new(vec![initial.clone(), left.clone(), right.clone()]).unwrap();
    assert!(graph.manual_migration("app", "manual", false).is_err());
    let merge = graph.manual_migration("app", "merge", true).unwrap();
    assert_eq!(merge.dependencies, [left.id.clone(), right.id.clone()]);
    let joined = MigrationGraph::new(vec![
        initial.clone(),
        left.clone(),
        right.clone(),
        merge.clone(),
    ])
    .unwrap();
    for applied in [left.id.clone(), right.id.clone()] {
        assert_eq!(
            joined
                .plan_forward(&[initial.id.clone(), applied].into(), Some(&merge.id))
                .unwrap()
                .len(),
            2
        );
    }
    let clashing = branch("0002_right", "a");
    assert!(
        MigrationGraph::new(vec![initial.clone(), left.clone(), clashing])
            .unwrap()
            .manual_migration("app", "merge", true)
            .is_err()
    );
    let sql = Migration::new(
        right.id,
        vec![initial.id.clone()],
        vec![MigrationOperation::RunSql {
            forward: "SELECT 1".into(),
            reverse: None,
            state_operations: vec![],
        }],
    );
    assert!(
        MigrationGraph::new(vec![initial, left, sql])
            .unwrap()
            .manual_migration("app", "merge", true)
            .unwrap_err()
            .to_string()
            .contains("merge requires review")
    );
}

#[test]
fn concurrent_backfills_cannot_merge_even_when_their_targets_differ() {
    use che_orm::{Migration, MigrationId, MigrationOperation};
    let mut state = ProjectState::from_app_schema(
        "app",
        &SchemaSet::new().model::<Parent>().model::<Unrelated>(),
    );
    for table in ["parents", "unrelated"] {
        let mut column = state.tables()[0].columns[0].clone();
        column.name = "value".into();
        column.field_name = "value".into();
        column.primary_key = false;
        column.nullable = true;
        MigrationOperation::AddColumn {
            table: table.into(),
            column,
        }
        .state_forwards(&mut state)
        .unwrap();
    }
    let initial = MigrationGraph::new(vec![])
        .unwrap()
        .generate(&state, MigrationLayout::PerApp, "initial")
        .unwrap()
        .remove(0);
    for (first, second) in [("0002_a", "0002_b"), ("0002_b", "0002_a")] {
        let branch = |name: &str, table: &str, value: &str| {
            Migration::new(
                MigrationId {
                    app: "app".into(),
                    name: name.into(),
                },
                vec![initial.id.clone()],
                vec![MigrationOperation::BackfillColumn {
                    table: table.into(),
                    column: "value".into(),
                    value: value.into(),
                }],
            )
        };
        let graph = MigrationGraph::new(vec![
            initial.clone(),
            branch(
                first,
                "parents",
                "coalesce((SELECT value FROM unrelated WHERE id = 1), 0)",
            ),
            branch(second, "unrelated", "7"),
        ])
        .unwrap();
        let error = graph
            .manual_migration("app", "merge", true)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("merge requires review")
                && error.contains("Backfill parents.value")
                && error.contains("Backfill unrelated.value"),
            "{error}"
        );
    }
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn reusing_index_names_after_an_already_applied_legacy_rename() {
    use che_orm::{Database, Migration, MigrationId, MigrationOperation};
    let mut old = ProjectState::from_app_schema("original", &SchemaSet::new().model::<Parent>());
    MigrationOperation::AddIndex {
        table: "parents".into(),
        columns: vec!["id".into()],
    }
    .state_forwards(&mut old)
    .unwrap();
    let empty = MigrationGraph::new(vec![]).unwrap();
    let mut files = empty
        .generate(&old, MigrationLayout::PerApp, "initial")
        .unwrap();
    let rename = MigrationOperation::RenameTable {
        old_name: "parents".into(),
        new_name: "archive".into(),
    };
    files.push(Migration::new(
        MigrationId {
            app: "original".into(),
            name: "0002_rename".into(),
        },
        vec![files[0].id.clone()],
        vec![rename],
    ));
    let recent = che_orm::SchemaObjectState {
        kind: che_orm::SchemaObjectKind::View,
        app: "original".into(),
        name: "recent".into(),
        sql: "CREATE VIEW recent AS SELECT id FROM archive".into(),
    };
    files.push(Migration::new(
        MigrationId {
            app: "original".into(),
            name: "0003_view".into(),
        },
        vec![files[1].id.clone()],
        vec![MigrationOperation::CreateSchemaObject { object: recent }],
    ));
    let history = MigrationGraph::new(files.clone()).unwrap();
    for claim in ["index", "model table", "SQL table", "view"] {
        let db = Database::connect_in_memory().await.unwrap();
        db.migrate(history.clone()).await.unwrap();
        db.execute_sql("INSERT INTO archive(id) VALUES (17)", vec![])
            .await
            .unwrap();
        let mut desired = history.replay(ProjectState::default()).unwrap();
        match claim {
            "index" | "model table" => {
                let mut replacement = old.models().remove(0);
                replacement.key.app = "new_app".into();
                if claim == "model table" {
                    replacement.table.name = "parents_idx_0".into();
                    replacement.table.indexes.clear();
                }
                MigrationOperation::CreateModel { model: replacement }
                    .state_forwards(&mut desired)
                    .unwrap();
            }
            _ => {
                let (kind, sql) = if claim == "SQL table" {
                    (
                        che_orm::SchemaObjectKind::Table,
                        "CREATE TABLE parents_idx_0 (id INTEGER PRIMARY KEY)",
                    )
                } else {
                    (
                        che_orm::SchemaObjectKind::View,
                        "CREATE VIEW parents_idx_0 AS SELECT 1 AS id",
                    )
                };
                desired
                    .add_schema_object(che_orm::SchemaObjectState {
                        kind,
                        app: "new_app".into(),
                        name: "parents_idx_0".into(),
                        sql: sql.into(),
                    })
                    .unwrap();
            }
        }
        let changes = history
            .generate(&desired, MigrationLayout::PerApp, "reuse")
            .unwrap();
        assert_eq!(changes.len(), 2);
        assert!(changes[1].dependencies.contains(&changes[0].id));
        let target = changes[1].id.clone();
        let repair = changes[0].id.clone();
        let graph = MigrationGraph::new(files.iter().cloned().chain(changes).collect()).unwrap();
        let sql = graph.migration_sql(&repair).unwrap();
        assert_eq!(sql.len(), 2);
        assert!(sql[0].starts_with("DROP INDEX") && sql[1].starts_with("CREATE INDEX"));
        assert_eq!(db.migrate_to(graph.clone(), Some(target)).await.unwrap(), 2);
        assert_eq!(
            db.query_sql("SELECT id FROM recent", vec![]).await.unwrap()[0]
                .get::<i64>(0)
                .unwrap(),
            17
        );
        assert_eq!(db.migrate(graph.clone()).await.unwrap(), 0);
        assert!(
            graph
                .generate(&desired, MigrationLayout::PerApp, "again")
                .unwrap()
                .is_empty()
        );
    }
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn renaming_table_to_its_own_old_index_name_checks_index_ownership() {
    use che_orm::{Database, MigrationOperation};
    let mut state = ProjectState::from_app_schema("app", &SchemaSet::new().model::<Parent>());
    MigrationOperation::AddIndex {
        table: "parents".into(),
        columns: vec!["id".into()],
    }
    .state_forwards(&mut state)
    .unwrap();
    let initial = MigrationGraph::new(vec![])
        .unwrap()
        .generate(&state, MigrationLayout::PerApp, "initial")
        .unwrap();
    let history = MigrationGraph::new(initial.clone()).unwrap();
    MigrationOperation::RenameTable {
        old_name: "parents".into(),
        new_name: "parents_idx_0".into(),
    }
    .state_forwards(&mut state)
    .unwrap();
    let files = history
        .generate(&state, MigrationLayout::PerApp, "rename")
        .unwrap();
    let graph = MigrationGraph::new(initial.into_iter().chain(files).collect()).unwrap();
    let db = Database::connect_in_memory().await.unwrap();
    db.migrate(history).await.unwrap();
    db.execute_sql("INSERT INTO parents(id) VALUES (13)", vec![])
        .await
        .unwrap();
    for sql in [
        "DROP INDEX parents_idx_0",
        "CREATE TABLE other(id INTEGER)",
        "CREATE INDEX parents_idx_0 ON other(id)",
    ] {
        db.execute_sql(sql, vec![]).await.unwrap();
    }
    assert!(
        db.migrate(graph.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("expected managed index")
    );
    assert_eq!(
        db.query_sql(
            "SELECT tbl_name FROM sqlite_master WHERE name='parents_idx_0'",
            vec![]
        )
        .await
        .unwrap()[0]
            .get::<String>(0)
            .unwrap(),
        "other"
    );
    assert_eq!(db.migration_history().await.unwrap().len(), 1);
    db.execute_sql("DROP INDEX parents_idx_0", vec![])
        .await
        .unwrap();
    db.execute_sql("CREATE INDEX parents_idx_0 ON parents(id)", vec![])
        .await
        .unwrap();
    assert_eq!(db.migrate(graph).await.unwrap(), 1);
    assert_eq!(
        db.query_sql("SELECT id FROM parents_idx_0", vec![])
            .await
            .unwrap()[0]
            .get::<i64>(0)
            .unwrap(),
        13
    );
}

#[test]
fn table_rename_requires_review_for_surviving_raw_foreign_keys_and_stale_views() {
    use che_orm::{MigrationError, MigrationOperation, SchemaObjectKind, SchemaObjectState};
    for (kind, name, sql) in [
        (
            SchemaObjectKind::Table,
            "raw_child",
            "CREATE TABLE raw_child (parent_id INTEGER REFERENCES \"project_parents\"(id))",
        ),
        (
            SchemaObjectKind::View,
            "parent_ids",
            "CREATE VIEW parent_ids AS SELECT id FROM \"project_parents\"",
        ),
    ] {
        let mut desired = ProjectState::from_app_schema("app", &SchemaSet::new().model::<Parent>());
        MigrationOperation::RenameTable {
            old_name: "parents".into(),
            new_name: "project_parents".into(),
        }
        .state_forwards(&mut desired)
        .unwrap();
        desired
            .add_schema_object(SchemaObjectState {
                app: "app".into(),
                kind,
                name: name.into(),
                sql: sql.into(),
            })
            .unwrap();
        let files = MigrationGraph::new(vec![])
            .unwrap()
            .generate(&desired, MigrationLayout::PerApp, "initial")
            .unwrap();
        let graph = MigrationGraph::new(files).unwrap();
        MigrationOperation::RenameTable {
            old_name: "project_parents".into(),
            new_name: "archive".into(),
        }
        .state_forwards(&mut desired)
        .unwrap();
        if kind == SchemaObjectKind::Table {
            let mut replacement = desired.models().remove(0);
            replacement.key.name = "Replacement".into();
            replacement.table.name = "project_parents".into();
            MigrationOperation::CreateModel { model: replacement }
                .state_forwards(&mut desired)
                .unwrap();
        }
        assert!(
            matches!(graph.generate(&desired, MigrationLayout::PerApp, "rename"),
            Err(MigrationError::NeedsManualMigration { resource, .. }) if resource == name)
        );
    }
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_rejects_colliding_column_names() {
    use che_orm::MigrationOperation;
    let original = ProjectState::from_app_schema("app", &SchemaSet::new().model::<Parent>());
    let files = MigrationGraph::new(vec![])
        .unwrap()
        .generate(&original, MigrationLayout::PerApp, "initial")
        .unwrap();
    let graph = MigrationGraph::new(files).unwrap();
    let mut column = original.tables()[0].columns[0].clone();
    column.name = "ID".into();
    column.primary_key = false;
    column.nullable = true;
    let mut changed = original;
    MigrationOperation::AddColumn {
        table: "parents".into(),
        column,
    }
    .state_forwards(&mut changed)
    .unwrap();
    let error = graph
        .generate(&changed, MigrationLayout::PerApp, "duplicate")
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("SQLite column name collision on parents")
    );
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn rebuild_restores_transitive_views_before_they_are_queried() {
    use che_orm::{Database, MigrationOperation, SchemaObjectKind, SchemaObjectState};
    let mut desired = ProjectState::from_app_schema("models", &SchemaSet::new().model::<Parent>());
    for (name, sql) in [
        ("base", "CREATE VIEW base AS SELECT id FROM parents"),
        ("outer_view", "CREATE VIEW outer_view AS SELECT * FROM base"),
    ] {
        desired
            .add_schema_object(SchemaObjectState {
                app: "views".into(),
                kind: SchemaObjectKind::View,
                name: name.into(),
                sql: sql.into(),
            })
            .unwrap();
    }
    let mut files = MigrationGraph::new(vec![])
        .unwrap()
        .generate(&desired, MigrationLayout::PerApp, "initial")
        .unwrap();
    let history = MigrationGraph::new(files.clone()).unwrap();
    let db = Database::connect_in_memory().await.unwrap();
    db.migrate(history.clone()).await.unwrap();
    db.execute_sql("INSERT INTO parents VALUES (42)", vec![])
        .await
        .unwrap();
    MigrationOperation::AddIndex {
        table: "parents".into(),
        columns: vec!["id".into()],
    }
    .state_forwards(&mut desired)
    .unwrap();
    files.extend(
        history
            .generate(&desired, MigrationLayout::PerApp, "index")
            .unwrap(),
    );
    let graph = MigrationGraph::new(files).unwrap();
    db.migrate(graph.clone()).await.unwrap();
    assert_eq!(
        db.query_sql("SELECT id FROM outer_view", vec![])
            .await
            .unwrap()[0]
            .get::<i64>(0)
            .unwrap(),
        42
    );
    assert_eq!(db.migrate(graph).await.unwrap(), 0);
}

#[test]
fn renamed_table_is_checked_against_final_view_sql_and_bare_fk() {
    use che_orm::{MigrationError, MigrationOperation, SchemaObjectKind, SchemaObjectState};
    let mut state = ProjectState::from_app_schema("models", &SchemaSet::new().model::<Parent>());
    MigrationOperation::RenameTable {
        old_name: "parents".into(),
        new_name: "work_items".into(),
    }
    .state_forwards(&mut state)
    .unwrap();
    let view = |sql: &str| SchemaObjectState {
        app: "reports".into(),
        name: "report".into(),
        kind: SchemaObjectKind::View,
        sql: sql.into(),
    };
    for foreign_key in [false, true] {
        let mut desired = state.clone();
        let object = if foreign_key {
            SchemaObjectState {
                app: "models".into(),
                name: "child".into(),
                kind: SchemaObjectKind::Table,
                sql: "CREATE TABLE child (pid INTEGER REFERENCES work_items(id))".into(),
            }
        } else {
            view("CREATE VIEW report AS SELECT id FROM work_items")
        };
        desired.add_schema_object(object.clone()).unwrap();
        let history = MigrationGraph::new(
            MigrationGraph::new(vec![])
                .unwrap()
                .generate(&desired, MigrationLayout::PerApp, "initial")
                .unwrap(),
        )
        .unwrap();
        MigrationOperation::RenameTable {
            old_name: "work_items".into(),
            new_name: "tickets".into(),
        }
        .state_forwards(&mut desired)
        .unwrap();
        if foreign_key {
            let mut replacement = state.models().remove(0);
            replacement.key.name = "Replacement".into();
            MigrationOperation::CreateModel { model: replacement }
                .state_forwards(&mut desired)
                .unwrap();
        } else {
            MigrationOperation::DropSchemaObject {
                kind: object.kind,
                name: object.name.clone(),
            }
            .state_forwards(&mut desired)
            .unwrap();
            desired
                .add_schema_object(view(
                    "CREATE VIEW report AS SELECT id AS task_id FROM work_items",
                ))
                .unwrap();
        }
        assert!(
            matches!(history.generate(&desired, MigrationLayout::PerApp,"rename"),
            Err(MigrationError::NeedsManualMigration {resource,..}) if resource == object.name)
        );
    }
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn target_reusing_an_index_name_applies_its_release_first() {
    use che_orm::{Database, MigrationOperation};
    let mut state = ProjectState::from_app_schema("z_owner", &SchemaSet::new().model::<Parent>());
    MigrationOperation::AddIndex {
        table: "parents".into(),
        columns: vec!["id".into()],
    }
    .state_forwards(&mut state)
    .unwrap();
    let mut files = MigrationGraph::new(vec![])
        .unwrap()
        .generate(&state, MigrationLayout::PerApp, "initial")
        .unwrap();
    let history = MigrationGraph::new(files.clone()).unwrap();
    MigrationOperation::RemoveIndex {
        table: "parents".into(),
        columns: vec!["id".into()],
    }
    .state_forwards(&mut state)
    .unwrap();
    let mut replacement = state.models().remove(0);
    replacement.key.app = "a_consumer".into();
    replacement.table.name = "parents_idx_0".into();
    MigrationOperation::CreateModel { model: replacement }
        .state_forwards(&mut state)
        .unwrap();
    let changes = history
        .generate(&state, MigrationLayout::PerApp, "reuse")
        .unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[1].id.app, "a_consumer");
    assert!(changes[1].dependencies.contains(&changes[0].id));
    let target = changes[1].id.clone();
    files.extend(changes);
    let graph = MigrationGraph::new(files).unwrap();
    let db = Database::connect_in_memory().await.unwrap();
    assert_eq!(db.migrate_to(graph.clone(), Some(target)).await.unwrap(), 3);
    db.execute_sql("INSERT INTO parents_idx_0 VALUES (1)", vec![])
        .await
        .unwrap();
    assert_eq!(db.migrate(graph).await.unwrap(), 0);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn literals_and_unrelated_column_names_do_not_create_dependencies() {
    use che_orm::{Database, MigrationOperation, SchemaObjectKind, SchemaObjectState};
    let mut state = ProjectState::from_app_schema("models", &SchemaSet::new().model::<Parent>());
    for (kind, name, sql) in [
        (
            SchemaObjectKind::View,
            "local_ids",
            "CREATE VIEW local_ids AS WITH parents AS (SELECT 7 AS id) SELECT id FROM parents",
        ),
        (
            SchemaObjectKind::Table,
            "log",
            "CREATE TABLE log (parents INTEGER)",
        ),
        (
            SchemaObjectKind::View,
            "first_view",
            "CREATE VIEW first_view AS SELECT 'FROM second_view' AS label",
        ),
        (
            SchemaObjectKind::View,
            "second_view",
            "CREATE VIEW IF NOT EXISTS second_view AS SELECT 'JOIN first_view' AS label /* FROM first_view, archive */",
        ),
    ] {
        state
            .add_schema_object(SchemaObjectState {
                app: "reports".into(),
                kind,
                name: name.into(),
                sql: sql.into(),
            })
            .unwrap();
    }
    let mut files = MigrationGraph::new(vec![])
        .unwrap()
        .generate(&state, MigrationLayout::PerApp, "initial")
        .unwrap();
    let history = MigrationGraph::new(files.clone()).unwrap();
    MigrationOperation::RenameTable {
        old_name: "parents".into(),
        new_name: "archive".into(),
    }
    .state_forwards(&mut state)
    .unwrap();
    files.extend(
        history
            .generate(&state, MigrationLayout::PerApp, "rename")
            .unwrap(),
    );
    let renamed = MigrationGraph::new(files.clone()).unwrap();
    MigrationOperation::AddIndex {
        table: "archive".into(),
        columns: vec!["id".into()],
    }
    .state_forwards(&mut state)
    .unwrap();
    files.extend(
        renamed
            .generate(&state, MigrationLayout::PerApp, "index")
            .unwrap(),
    );
    let db = Database::connect_in_memory().await.unwrap();
    db.migrate(MigrationGraph::new(files).unwrap())
        .await
        .unwrap();
    assert_eq!(
        db.query_sql("SELECT id FROM local_ids", vec![])
            .await
            .unwrap()[0]
            .get::<i64>(0)
            .unwrap(),
        7
    );
    db.execute_sql("INSERT INTO log(parents) VALUES (42)", vec![])
        .await
        .unwrap();
    assert_eq!(
        db.query_sql("SELECT label FROM second_view", vec![])
            .await
            .unwrap()[0]
            .get::<String>(0)
            .unwrap(),
        "JOIN first_view"
    );
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn grouped_from_reads_replacement_after_table_rename() {
    use che_orm::{Database, MigrationOperation, SchemaObjectKind, SchemaObjectState};
    let mut state = ProjectState::from_app_schema(
        "app",
        &SchemaSet::new().model::<Parent>().model::<Unrelated>(),
    );
    state
        .add_schema_object(SchemaObjectState {
            app: "app".into(),
            kind: SchemaObjectKind::View,
            name: "report".into(),
            sql: "CREATE VIEW report AS SELECT parents.id FROM (unrelated, parents)".into(),
        })
        .unwrap();
    let mut files = MigrationGraph::new(vec![])
        .unwrap()
        .generate(&state, MigrationLayout::PerApp, "initial")
        .unwrap();
    let history = MigrationGraph::new(files.clone()).unwrap();
    let db = Database::connect_in_memory().await.unwrap();
    db.migrate(history.clone()).await.unwrap();
    db.execute_sql("INSERT INTO unrelated VALUES (1)", vec![])
        .await
        .unwrap();
    db.execute_sql("INSERT INTO parents VALUES (10)", vec![])
        .await
        .unwrap();
    let mut replacement = state
        .models()
        .into_iter()
        .find(|m| m.table.name == "parents")
        .unwrap();
    replacement.key.name = "Replacement".into();
    MigrationOperation::RenameTable {
        old_name: "parents".into(),
        new_name: "archive".into(),
    }
    .state_forwards(&mut state)
    .unwrap();
    MigrationOperation::CreateModel { model: replacement }
        .state_forwards(&mut state)
        .unwrap();
    files.extend(
        history
            .generate(&state, MigrationLayout::PerApp, "rename")
            .unwrap(),
    );
    db.migrate(MigrationGraph::new(files).unwrap())
        .await
        .unwrap();
    db.execute_sql("INSERT INTO parents VALUES (20)", vec![])
        .await
        .unwrap();
    assert_eq!(
        db.query_sql("SELECT id FROM report", vec![]).await.unwrap()[0]
            .get::<i64>(0)
            .unwrap(),
        20
    );
}

#[cfg(feature = "sqlite")]
#[test]
fn lateral_is_a_sqlite_fk_target() {
    use che_orm::{MigrationError, MigrationOperation, SchemaObjectKind, SchemaObjectState};
    let mut state = ProjectState::from_app_schema("app", &SchemaSet::new().model::<Parent>());
    MigrationOperation::RenameTable {
        old_name: "parents".into(),
        new_name: "lateral".into(),
    }
    .state_forwards(&mut state)
    .unwrap();
    state
        .add_schema_object(SchemaObjectState {
            app: "app".into(),
            kind: SchemaObjectKind::Table,
            name: "children".into(),
            sql: "CREATE TABLE children (pid INTEGER REFERENCES lateral(id))".into(),
        })
        .unwrap();
    let files = MigrationGraph::new(vec![])
        .unwrap()
        .generate(&state, MigrationLayout::PerApp, "initial")
        .unwrap();
    let mut replacement = state.models().remove(0);
    replacement.key.name = "Replacement".into();
    MigrationOperation::RenameTable {
        old_name: "lateral".into(),
        new_name: "archive".into(),
    }
    .state_forwards(&mut state)
    .unwrap();
    MigrationOperation::CreateModel { model: replacement }
        .state_forwards(&mut state)
        .unwrap();
    assert!(
        matches!(MigrationGraph::new(files).unwrap().generate(&state,MigrationLayout::PerApp,"rename"),
        Err(MigrationError::NeedsManualMigration{resource,..}) if resource=="children")
    );
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite_rebuild_preserves_tables_using_scratch_names() {
    use che_orm::{Database, MigrationOperation};
    let mut state = ProjectState::from_app_schema("app", &SchemaSet::new().model::<Parent>());
    let mut reserved = state.models().remove(0);
    reserved.key.name = "Reserved".into();
    reserved.table.name = "__che_migration_new_parents".into();
    MigrationOperation::CreateModel { model: reserved }
        .state_forwards(&mut state)
        .unwrap();
    let mut files = MigrationGraph::new(vec![])
        .unwrap()
        .generate(&state, MigrationLayout::PerApp, "initial")
        .unwrap();
    let history = MigrationGraph::new(files.clone()).unwrap();
    let db = Database::connect_in_memory().await.unwrap();
    db.migrate(history.clone()).await.unwrap();
    db.execute_sql(
        "CREATE TABLE __che_migration_new_parents_1 (id INTEGER)",
        vec![],
    )
    .await
    .unwrap();
    for table in [
        "parents",
        "__che_migration_new_parents",
        "__che_migration_new_parents_1",
    ] {
        db.execute_sql(format!("INSERT INTO {table} VALUES (8)"), vec![])
            .await
            .unwrap();
    }
    MigrationOperation::AddIndex {
        table: "parents".into(),
        columns: vec!["id".into()],
    }
    .state_forwards(&mut state)
    .unwrap();
    let changes = history
        .generate(&state, MigrationLayout::PerApp, "index")
        .unwrap();
    let id = changes[0].id.clone();
    files.extend(changes);
    let graph = MigrationGraph::new(files).unwrap();
    assert!(graph.migration_sql(&id).unwrap()[0].contains("__che_migration_new_parents_1"));
    db.migrate(graph.clone()).await.unwrap();
    assert_eq!(db.migrate(graph).await.unwrap(), 0);
    for table in [
        "parents",
        "__che_migration_new_parents",
        "__che_migration_new_parents_1",
    ] {
        assert_eq!(
            db.query_sql(format!("SELECT id FROM {table}"), vec![])
                .await
                .unwrap()[0]
                .get::<i64>(0)
                .unwrap(),
            8
        );
    }
}

#[test]
fn column_rename_checks_final_view_chain_and_keeps_other_qualified_columns() {
    use che_orm::{
        GenerationPolicy, MigrationError, MigrationOperation, SchemaObjectKind, SchemaObjectState,
    };
    let mut state = ProjectState::from_app_schema(
        "app",
        &SchemaSet::new().model::<Parent>().model::<Unrelated>(),
    );
    let view = |name: &str, sql: &str| SchemaObjectState {
        app: "app".into(),
        kind: SchemaObjectKind::View,
        name: name.into(),
        sql: sql.into(),
    };
    for v in [
        view("inner_v", "CREATE VIEW inner_v AS SELECT * FROM parents"),
        view("outer_v", "CREATE VIEW outer_v AS SELECT id FROM inner_v"),
        view(
            "alias_report",
            "CREATE VIEW alias_report AS SELECT u.id FROM unrelated AS u CROSS JOIN parents AS p",
        ),
        view(
            "report",
            "CREATE VIEW report AS SELECT unrelated.id FROM unrelated CROSS JOIN parents",
        ),
    ] {
        state.add_schema_object(v).unwrap();
    }
    let files = MigrationGraph::new(vec![])
        .unwrap()
        .generate(&state, MigrationLayout::PerApp, "initial")
        .unwrap();
    let graph = MigrationGraph::new(files.clone()).unwrap();
    MigrationOperation::RenameColumn {
        table: "parents".into(),
        old_name: "id".into(),
        new_name: "pk".into(),
    }
    .state_forwards(&mut state)
    .unwrap();
    let drop_outer = MigrationOperation::DropSchemaObject {
        kind: SchemaObjectKind::View,
        name: "outer_v".into(),
    };
    drop_outer.state_forwards(&mut state).unwrap();
    state
        .add_schema_object(view(
            "outer_v",
            "CREATE VIEW outer_v AS SELECT id AS item FROM inner_v",
        ))
        .unwrap();
    let policy = GenerationPolicy {
        rename_columns: [(("parents".into(), "id".into()), "pk".into())].into(),
        ..Default::default()
    };
    let generate = |state: &ProjectState| {
        graph.generate_with_policy(state, MigrationLayout::PerApp, "rename", &policy)
    };
    assert!(
        matches!(generate(&state), Err(MigrationError::NeedsManualMigration{resource,..}) if resource=="outer_v")
    );
    drop_outer.state_forwards(&mut state).unwrap();
    state
        .add_schema_object(view(
            "outer_v",
            "CREATE VIEW outer_v AS SELECT pk AS item FROM inner_v",
        ))
        .unwrap();
    // The outer view can also keep reading id if the inner view exports it.
    let mut reexport = state.clone();
    MigrationOperation::DropSchemaObject {
        kind: SchemaObjectKind::View,
        name: "inner_v".into(),
    }
    .state_forwards(&mut reexport)
    .unwrap();
    reexport
        .add_schema_object(view(
            "inner_v",
            "CREATE VIEW inner_v AS SELECT pk AS id FROM parents",
        ))
        .unwrap();
    drop_outer.state_forwards(&mut reexport).unwrap();
    reexport
        .add_schema_object(view(
            "outer_v",
            "CREATE VIEW outer_v AS SELECT id FROM inner_v",
        ))
        .unwrap();
    generate(&reexport).unwrap();
    let changes = generate(&state).unwrap();
    assert!(!changes.is_empty());
    #[cfg(feature = "sqlite")]
    {
        use che_orm::migration::sqlite_executor::apply_operation_migrations_on_connection as apply;
        let mut db = che_orm::rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut db, graph).unwrap();
        db.execute_batch("INSERT INTO parents VALUES (41); INSERT INTO unrelated VALUES (99)")
            .unwrap();
        let final_graph = MigrationGraph::new(files.into_iter().chain(changes).collect()).unwrap();
        apply(&mut db, final_graph.clone()).unwrap();
        assert_eq!(
            db.query_row("SELECT item FROM outer_v", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            41
        );
        assert_eq!(
            db.query_row("SELECT id FROM report", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            99
        );
        assert_eq!(apply(&mut db, final_graph).unwrap(), 0);
    }
}

#[cfg(feature = "postgres")]
#[test]
fn escaped_string_default_does_not_hide_postgres_fk() {
    use che_orm::{MigrationError, MigrationOperation, SchemaObjectKind, SchemaObjectState};
    let mut state = ProjectState::from_app_schema("app", &SchemaSet::new().model::<Parent>());
    state.add_schema_object(SchemaObjectState{app:"app".into(),kind:SchemaObjectKind::Table,name:"child".into(),
        sql:r"CREATE TABLE child (label text DEFAULT E'it\'s', pid bigint REFERENCES parents(id))".into()}).unwrap();
    let files = MigrationGraph::new(vec![])
        .unwrap()
        .generate(&state, MigrationLayout::PerApp, "initial")
        .unwrap();
    let mut replacement = state.models().remove(0);
    replacement.key.name = "Replacement".into();
    MigrationOperation::RenameTable {
        old_name: "parents".into(),
        new_name: "archive".into(),
    }
    .state_forwards(&mut state)
    .unwrap();
    MigrationOperation::CreateModel { model: replacement }
        .state_forwards(&mut state)
        .unwrap();
    assert!(
        matches!(MigrationGraph::new(files).unwrap().generate(&state,MigrationLayout::PerApp,"rename"),
        Err(MigrationError::NeedsManualMigration{resource,..}) if resource=="child")
    );
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_rename_and_rebuild_use_main_despite_temp_shadows() {
    use che_orm::migration::sqlite_executor::apply_operation_migrations_on_connection as apply;
    use che_orm::{MigrationOperation, rusqlite::Connection};
    let mut state = ProjectState::from_app_schema("app", &SchemaSet::new().model::<Parent>());
    let mut files = MigrationGraph::new(vec![])
        .unwrap()
        .generate(&state, MigrationLayout::PerApp, "initial")
        .unwrap();
    let graph = MigrationGraph::new(files.clone()).unwrap();
    let mut db = Connection::open_in_memory().unwrap();
    apply(&mut db, graph.clone()).unwrap();
    db.execute_batch(
        "INSERT INTO parents VALUES (41);
        CREATE TEMP TABLE parents(id INTEGER); INSERT INTO temp.parents VALUES (99);
        CREATE TEMP TABLE archive(pk INTEGER); INSERT INTO temp.archive VALUES (77);
        CREATE TEMP TABLE che_migration_history(dummy TEXT);",
    )
    .unwrap();
    MigrationOperation::RenameTable {
        old_name: "parents".into(),
        new_name: "archive".into(),
    }
    .state_forwards(&mut state)
    .unwrap();
    MigrationOperation::AddIndex {
        table: "archive".into(),
        columns: vec!["id".into()],
    }
    .state_forwards(&mut state)
    .unwrap();
    state
        .add_schema_object(che_orm::SchemaObjectState {
            app: "app".into(),
            name: "track".into(),
            kind: che_orm::SchemaObjectKind::Trigger,
            sql: "CREATE TRIGGER track AFTER INSERT ON archive BEGIN SELECT 1; END".into(),
        })
        .unwrap();
    files.extend(
        graph
            .generate(&state, MigrationLayout::PerApp, "rename")
            .unwrap(),
    );
    let graph = MigrationGraph::new(files).unwrap();
    assert_eq!(apply(&mut db, graph.clone()).unwrap(), 1);
    assert_eq!(apply(&mut db, graph).unwrap(), 0);
    assert_eq!(
        db.query_row(
            "SELECT tbl_name FROM main.sqlite_schema WHERE name='track'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "archive"
    );
    for (table, value) in [
        ("main.archive", 41),
        ("temp.parents", 99),
        ("temp.archive", 77),
    ] {
        assert_eq!(
            db.query_row(&format!("SELECT * FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            value
        );
    }
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_checks_final_view_exports_and_registered_sql_tables() {
    use che_orm::migration::sqlite_executor::apply_operation_migrations_on_connection as apply;
    use che_orm::{
        GenerationPolicy, MigrationError, MigrationOperation, SchemaObjectKind, SchemaObjectState,
        rusqlite::Connection,
    };
    let mut state = ProjectState::from_app_schema("app", &SchemaSet::new().model::<Parent>());
    for (kind, name, sql) in [
        (
            SchemaObjectKind::Table,
            "raw",
            "CREATE TABLE raw (tag TEXT)",
        ),
        (
            SchemaObjectKind::View,
            "inner_v",
            "CREATE VIEW inner_v AS SELECT id AS amount FROM parents CROSS JOIN raw",
        ),
        (
            SchemaObjectKind::View,
            "outer_v",
            "CREATE VIEW outer_v AS SELECT amount FROM inner_v",
        ),
    ] {
        state
            .add_schema_object(SchemaObjectState {
                app: "app".into(),
                kind,
                name: name.into(),
                sql: sql.into(),
            })
            .unwrap();
    }
    let initial = MigrationGraph::new(vec![])
        .unwrap()
        .generate(&state, MigrationLayout::PerApp, "initial")
        .unwrap();
    let history = MigrationGraph::new(initial.clone()).unwrap();
    MigrationOperation::RenameColumn {
        table: "parents".into(),
        old_name: "id".into(),
        new_name: "pk".into(),
    }
    .state_forwards(&mut state)
    .unwrap();
    let policy = GenerationPolicy {
        rename_columns: [(("parents".into(), "id".into()), "pk".into())].into(),
        ..Default::default()
    };
    let generate = |state: &ProjectState| {
        history.generate_with_policy(state, MigrationLayout::PerApp, "rename", &policy)
    };
    let drop_inner = MigrationOperation::DropSchemaObject {
        kind: SchemaObjectKind::View,
        name: "inner_v".into(),
    };
    let drop_outer = MigrationOperation::DropSchemaObject {
        kind: SchemaObjectKind::View,
        name: "outer_v".into(),
    };
    drop_inner.state_forwards(&mut state).unwrap();
    state
        .add_schema_object(SchemaObjectState {
            app: "app".into(),
            kind: SchemaObjectKind::View,
            name: "inner_v".into(),
            sql: "CREATE VIEW inner_v AS SELECT pk AS total FROM parents CROSS JOIN raw".into(),
        })
        .unwrap();
    assert!(
        matches!(generate(&state), Err(MigrationError::NeedsManualMigration { resource, .. }) if resource == "outer_v")
    );
    drop_inner.state_forwards(&mut state).unwrap();
    drop_outer.state_forwards(&mut state).unwrap();
    for (name, sql) in [
        (
            "inner_v",
            "CREATE VIEW inner_v AS SELECT pk AS id FROM parents CROSS JOIN raw",
        ),
        ("outer_v", "CREATE VIEW outer_v AS SELECT id FROM inner_v"),
    ] {
        state
            .add_schema_object(SchemaObjectState {
                app: "app".into(),
                kind: SchemaObjectKind::View,
                name: name.into(),
                sql: sql.into(),
            })
            .unwrap();
    }
    let changes = generate(&state).unwrap();
    let mut db = Connection::open_in_memory().unwrap();
    apply(&mut db, history).unwrap();
    db.execute_batch("INSERT INTO parents VALUES (41); INSERT INTO raw VALUES ('fixture')")
        .unwrap();
    let graph = MigrationGraph::new(initial.into_iter().chain(changes).collect()).unwrap();
    apply(&mut db, graph.clone()).unwrap();
    assert_eq!(
        db.query_row("SELECT id FROM outer_v", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        41
    );
    assert_eq!(apply(&mut db, graph).unwrap(), 0);
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_checks_dependent_views_before_removing_allowed_columns() {
    use che_orm::{
        GenerationPolicy, MigrationError, MigrationOperation, SchemaObjectKind, SchemaObjectState,
    };
    let mut state = ProjectState::from_app_schema("app", &SchemaSet::new().model::<Parent>());
    let mut column = state.tables()[0].columns[0].clone();
    column.field_name = "label".into();
    column.name = "label".into();
    column.primary_key = false;
    column.nullable = true;
    MigrationOperation::AddColumn {
        table: "parents".into(),
        column,
    }
    .state_forwards(&mut state)
    .unwrap();
    state
        .add_schema_object(SchemaObjectState {
            app: "app".into(),
            kind: SchemaObjectKind::View,
            name: "report".into(),
            sql: "CREATE VIEW report AS SELECT label FROM parents".into(),
        })
        .unwrap();
    let initial = MigrationGraph::new(vec![])
        .unwrap()
        .generate(&state, MigrationLayout::PerApp, "initial")
        .unwrap();
    let history = MigrationGraph::new(initial).unwrap();
    MigrationOperation::RemoveColumn {
        table: "parents".into(),
        column: "label".into(),
    }
    .state_forwards(&mut state)
    .unwrap();
    let policy = GenerationPolicy {
        allow_drop: ["parents.label".into()].into(),
        ..Default::default()
    };
    let generate = |state: &ProjectState| {
        history.generate_with_policy(state, MigrationLayout::PerApp, "remove", &policy)
    };
    assert!(
        matches!(generate(&state), Err(MigrationError::NeedsManualMigration { resource, .. }) if resource == "report")
    );
    MigrationOperation::DropSchemaObject {
        kind: SchemaObjectKind::View,
        name: "report".into(),
    }
    .state_forwards(&mut state)
    .unwrap();
    assert!(!generate(&state).unwrap().is_empty());
}

#[test]
fn migrations_reject_invalid_table_names_before_changing_state() {
    use che_orm::{MigrationOperation, SchemaObjectKind, SchemaObjectState};
    let original = ProjectState::from_app_schema("app", &SchemaSet::new().model::<Parent>());
    for name in [
        "Parents",
        "parent-items",
        "\"parents\"",
        "1parents",
        "parent items",
        "задачи",
    ] {
        let mut state = original.clone();
        let mut model = state.models().remove(0);
        model.key.name = "Another".into();
        model.table.name = name.into();
        let create = MigrationOperation::CreateModel { model };
        let rename = MigrationOperation::RenameTable {
            old_name: "parents".into(),
            new_name: name.into(),
        };
        for operation in [create, rename] {
            let error = operation
                .state_forwards(&mut state)
                .unwrap_err()
                .to_string();
            assert!(error.contains("expected [a-z_][a-z0-9_]*"), "{error}");
            assert_eq!(state, original);
        }
    }

    let mut state = original.clone();
    let object = |sql: &str| SchemaObjectState {
        app: "app".into(),
        kind: SchemaObjectKind::Table,
        name: "audit_1".into(),
        sql: sql.into(),
    };
    assert!(
        state
            .add_schema_object(object("CREATE TABLE \"\"\"audit_1\"\"\" (id INTEGER)"))
            .is_err()
    );
    assert!(
        state
            .add_schema_object(object("CREATE TABLE Audit_1 (id INTEGER)"))
            .is_err()
    );
    assert_eq!(state, original);
    state
        .add_schema_object(object("CREATE TABLE \"audit_1\"(id INTEGER)"))
        .unwrap();
}

#[test]
fn planner_requires_lowercase_table_names_in_options_and_contracts() {
    use che_orm::{GenerationPolicy, SchemaObjectKind, SchemaObjectState, SqlTableContract};

    let mut desired = ProjectState::from_app_schema("app", &SchemaSet::new().model::<Parent>());
    desired
        .add_schema_object(SchemaObjectState {
            app: "app".into(),
            kind: SchemaObjectKind::Table,
            name: "audit".into(),
            sql: "CREATE TABLE audit (id INTEGER PRIMARY KEY)".into(),
        })
        .unwrap();

    let graph = MigrationGraph::new(vec![]).unwrap();
    let invalid_policies = [
        GenerationPolicy {
            allow_drop: ["Parents.id".into()].into(),
            ..Default::default()
        },
        GenerationPolicy {
            rename_columns: [(("\"parents\"".into(), "id".into()), "pk".into())].into(),
            ..Default::default()
        },
        GenerationPolicy {
            external_keys: [("external-parent".into(), "id".into())].into(),
            ..Default::default()
        },
        GenerationPolicy {
            sql_tables: [("Audit".into(), SqlTableContract::default())].into(),
            ..Default::default()
        },
        GenerationPolicy {
            sql_tables: [(
                "audit".into(),
                SqlTableContract {
                    requires: [("Parents".into(), "id".into())].into(),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        },
    ];
    for policy in invalid_policies {
        let error = graph
            .generate_with_policy(&desired, MigrationLayout::PerApp, "initial", &policy)
            .unwrap_err();
        assert!(
            error.to_string().contains("expected [a-z_][a-z0-9_]*"),
            "{error}"
        );
    }
}

#[test]
fn registered_schema_objects_require_one_create_statement() {
    use che_orm::{SchemaObjectKind, SchemaObjectState};
    let mut state = ProjectState::default();
    let table = |sql: &str| SchemaObjectState {
        app: "app".into(),
        kind: SchemaObjectKind::Table,
        name: "safe_table".into(),
        sql: sql.into(),
    };
    let error = state
        .add_schema_object(table(
            "CREATE TABLE safe_table (id INTEGER); CREATE TABLE \"Bad-Name\" (id INTEGER)",
        ))
        .unwrap_err();
    assert!(error.to_string().contains("exactly one CREATE statement"));
    assert!(state.objects().is_empty());
    state
        .add_schema_object(table(
            "CREATE TABLE safe_table (label TEXT DEFAULT ';'); -- trailing comment",
        ))
        .unwrap();

    #[cfg(feature = "sqlite")]
    state
        .add_schema_object(SchemaObjectState {
            app: "app".into(),
            kind: SchemaObjectKind::Trigger,
            name: "safe_insert".into(),
            sql: "CREATE TRIGGER safe_insert AFTER INSERT ON safe_table \
                  BEGIN SELECT ';'; SELECT 1; END; -- trailing comment"
                .into(),
        })
        .unwrap();

    #[cfg(feature = "postgres")]
    state
        .add_schema_object(SchemaObjectState {
            app: "app".into(),
            kind: SchemaObjectKind::View,
            name: "safe_report".into(),
            sql: "CREATE VIEW safe_report AS SELECT ARRAY[1] AS ids; /* outer /* inner */ ; comment */"
                .into(),
        })
        .unwrap();
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn target_rebuild_restores_unchanged_cross_app_view_in_the_same_migration() {
    use che_orm::{Database, MigrationOperation, SchemaObjectKind, SchemaObjectState};
    let mut desired = ProjectState::from_app_schema("models", &SchemaSet::new().model::<Parent>());
    desired
        .add_schema_object(SchemaObjectState {
            app: "reports".into(),
            kind: SchemaObjectKind::View,
            name: "parent_report".into(),
            sql: "CREATE VIEW parent_report AS SELECT id FROM parents".into(),
        })
        .unwrap();
    let mut files = MigrationGraph::new(vec![])
        .unwrap()
        .generate_initial(&desired, "initial")
        .unwrap();
    let historical = MigrationGraph::new(files.clone()).unwrap();
    let db = Database::connect_in_memory().await.unwrap();
    db.migrate(historical.clone()).await.unwrap();
    db.execute_sql("INSERT INTO parents VALUES (42)", vec![])
        .await
        .unwrap();
    MigrationOperation::AddIndex {
        table: "parents".into(),
        columns: vec!["id".into()],
    }
    .state_forwards(&mut desired)
    .unwrap();

    let changes = historical
        .generate(&desired, MigrationLayout::PerApp, "index")
        .unwrap();
    let [change] = changes.as_slice() else {
        panic!("expected one atomic migration")
    };
    assert_eq!(change.id.app, "models");
    assert!(matches!(
        change.operations.as_slice(),
        [
            MigrationOperation::DropSchemaObject { .. },
            MigrationOperation::AddIndex { .. },
            MigrationOperation::CreateSchemaObject { .. },
        ]
    ));
    let mut changed_view = desired.clone();
    let mut view = changed_view.objects()[0].clone();
    MigrationOperation::DropSchemaObject {
        kind: view.kind,
        name: view.name.clone(),
    }
    .state_forwards(&mut changed_view)
    .unwrap();
    view.sql = "CREATE VIEW parent_report AS SELECT id + 1 AS id FROM parents".into();
    changed_view.add_schema_object(view).unwrap();
    let changes_with_view = historical
        .generate(&changed_view, MigrationLayout::PerApp, "index")
        .unwrap();
    let [batch, reports_target] = changes_with_view.as_slice() else {
        panic!("expected the atomic batch and a reports target")
    };
    assert_eq!(batch.id.app, "models");
    assert_eq!(reports_target.id.app, "reports");
    assert!(reports_target.operations.is_empty());
    assert!(reports_target.dependencies.contains(&batch.id));

    let target = change.id.clone();
    files.extend(changes);
    let graph = MigrationGraph::new(files).unwrap();
    assert_eq!(db.migrate_to(graph.clone(), Some(target)).await.unwrap(), 1);
    assert_eq!(
        db.query_sql("SELECT id FROM parent_report", vec![])
            .await
            .unwrap()[0]
            .get::<i64>(0)
            .unwrap(),
        42
    );
    assert_eq!(db.migrate(graph).await.unwrap(), 0);
}

#[test]
fn initial_and_scoped_generation_resolve_foreign_keys_to_registered_sql_tables() {
    use che_orm::{GenerationPolicy, SchemaObjectKind, SchemaObjectState};
    let mut desired =
        ProjectState::from_app_schema("consumer", &SchemaSet::new().model::<CaseChild>());
    desired
        .add_schema_object(SchemaObjectState {
            app: "provider".into(),
            kind: SchemaObjectKind::Table,
            name: "raw_parent".into(),
            sql: "CREATE TABLE raw_parent (\"ID\" INTEGER PRIMARY KEY)".into(),
        })
        .unwrap();
    let empty = MigrationGraph::new(vec![]).unwrap();
    let files = empty.generate_initial(&desired, "initial").unwrap();
    let [provider, consumer] = files.as_slice() else {
        panic!("expected two migrations")
    };
    assert_eq!(provider.id.app, "provider");
    assert_eq!(consumer.dependencies, vec![provider.id.clone()]);
    let policy = GenerationPolicy::default();
    let error = empty
        .generate_for_app(&desired, "consumer", "initial", &policy)
        .unwrap_err();
    assert!(error.to_string().contains("generate and register it first"));
    let scoped = MigrationGraph::new(vec![provider.clone()])
        .unwrap()
        .generate_for_app(&desired, "consumer", "initial", &policy)
        .unwrap();
    assert_eq!(scoped[0].dependencies, vec![provider.id.clone()]);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn initial_migrations_order_cross_app_objects_by_actual_dependencies() {
    use che_orm::{Database, SchemaObjectKind, SchemaObjectState};
    let mut state = ProjectState::default();
    for (app, kind, name, sql) in [
        (
            "a",
            SchemaObjectKind::Table,
            "a_data",
            "CREATE TABLE a_data (id INTEGER PRIMARY KEY)",
        ),
        (
            "a",
            SchemaObjectKind::View,
            "a_report",
            "CREATE VIEW a_report AS SELECT id FROM b_data",
        ),
        (
            "b",
            SchemaObjectKind::Table,
            "b_data",
            "CREATE TABLE b_data (id INTEGER PRIMARY KEY)",
        ),
        (
            "b",
            SchemaObjectKind::View,
            "b_report",
            "CREATE VIEW b_report AS SELECT id FROM a_data",
        ),
    ] {
        state
            .add_schema_object(SchemaObjectState {
                app: app.into(),
                kind,
                name: name.into(),
                sql: sql.into(),
            })
            .unwrap();
    }
    let files = MigrationGraph::new(vec![])
        .unwrap()
        .generate_initial(&state, "initial")
        .unwrap();
    let db = Database::connect_in_memory().await.unwrap();
    db.migrate(MigrationGraph::new(files).unwrap())
        .await
        .unwrap();
    for view in ["a_report", "b_report"] {
        db.query_sql(format!("SELECT id FROM {view}"), vec![])
            .await
            .unwrap();
    }
}
