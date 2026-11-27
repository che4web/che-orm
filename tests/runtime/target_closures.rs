//! Compare the rows seen after targeted installation with full deployment.
use super::{Database, Model, fixture};
use che_orm::{
    GenerationPolicy, Migration, MigrationGraph, MigrationLayout, MigrationOperation as Op,
    ProjectState, SchemaObjectKind, SchemaObjectState, SchemaRequirement as Req, SchemaSet,
};
use std::error::Error;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Debug, Model)]
#[orm(table = "items")]
struct Item {
    #[orm(primary_key, check = "id > 0")]
    id: i64,
}

#[derive(Debug, Model)]
#[orm(table = "details")]
struct Detail {
    #[orm(primary_key)]
    id: i64,
    private_note: Option<i64>,
}

#[derive(Debug, Model)]
#[orm(table = "catalog_a")]
struct CatalogA {
    #[orm(primary_key)]
    id: i64,
    #[orm(references = "parent_a(id)")]
    parent_id: Option<i64>,
}

#[derive(Debug, Model)]
#[orm(table = "catalog_b")]
struct CatalogB {
    #[orm(primary_key)]
    id: i64,
    #[orm(references = "parent_b(id)")]
    parent_id: Option<i64>,
}

#[derive(Debug, Model)]
#[orm(table = "parent_b")]
struct LegacyB {
    #[orm(primary_key)]
    id: i64,
}

fn table(app: &str, name: &str) -> SchemaObjectState {
    SchemaObjectState {
        app: app.into(),
        kind: SchemaObjectKind::Table,
        name: name.into(),
        sql: format!("CREATE TABLE {name}(id BIGINT PRIMARY KEY)"),
    }
}

fn report(name: &str, select: &str) -> SchemaObjectState {
    SchemaObjectState {
        app: "reports".into(),
        kind: SchemaObjectKind::View,
        name: name.into(),
        sql: format!("CREATE VIEW {name} AS {select}"),
    }
}

fn add_column(state: &mut ProjectState, name: &str) -> Result<()> {
    let items = state.tables().iter().find(|t| t.name == "items").unwrap();
    let mut column = items.columns[0].clone();
    column.name = name.into();
    column.field_name = name.into();
    column.primary_key = false;
    column.check = None;
    column.nullable = true;
    Op::AddColumn {
        table: "items".into(),
        column,
    }
    .state_forwards(state)?;
    Ok(())
}

fn alter_column(
    state: &mut ProjectState,
    name: &str,
    edit: impl FnOnce(&mut che_orm::ColumnState),
) -> Result<()> {
    let table = state.tables().iter().find(|t| t.name == "items").unwrap();
    let old_column = table
        .columns
        .iter()
        .find(|c| c.name == name)
        .unwrap()
        .clone();
    let mut new_column = old_column.clone();
    edit(&mut new_column);
    Op::AlterColumn {
        table: table.name.clone(),
        old_column,
        new_column,
    }
    .state_forwards(state)?;
    Ok(())
}

fn graph(files: &[Migration]) -> Result<MigrationGraph> {
    Ok(MigrationGraph::new(files.to_vec())?)
}

fn baseline(state: &ProjectState) -> Result<Vec<Migration>> {
    Ok(graph(&[])?.generate_initial(state, "initial")?)
}

fn update(files: &mut Vec<Migration>, desired: &ProjectState, drop: &str) -> Result<()> {
    let mut policy = GenerationPolicy::default();
    if !drop.is_empty() {
        policy.allow_drop.insert(drop.into());
    }
    files.extend(graph(files)?.generate_with_policy(
        desired,
        MigrationLayout::PerApp,
        "update",
        &policy,
    )?);
    Ok(())
}

async fn install(db: &Database, files: &[Migration], app: &str) -> Result<()> {
    let graph = graph(files)?;
    let heads = graph.app_heads(app);
    assert_eq!(heads.len(), 1);
    let target = Some(heads[0].id.clone());
    db.migrate_to(graph.clone(), target.clone()).await?;
    assert_eq!(db.migrate_to(graph, target).await?, 0);
    Ok(())
}

async fn value(db: &Database, sql: &str) -> Result<i64> {
    Ok(db.query_sql(sql, vec![]).await?[0].get::<i64>(0)?)
}

async fn cleanup(db: Option<(Database, String)>) -> Result<()> {
    if let Some((admin, schema)) = db {
        admin
            .execute_sql(format!("DROP SCHEMA \"{schema}\" CASCADE"), vec![])
            .await?;
    }
    Ok(())
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn reports_keep_independent_columns_through_additions_and_qualified_stars() -> Result<()> {
    for qualified_star in [false, true] {
        let mut old = ProjectState::from_app_schema(
            "stock",
            &SchemaSet::new().model::<Item>().model::<Detail>(),
        );
        add_column(&mut old, "private_note")?;
        let source = if qualified_star { "details" } else { "items" };
        Op::AddIndex {
            table: source.into(),
            columns: vec!["private_note".into()],
        }
        .state_forwards(&mut old)?;
        let mut desired = old.clone();
        Op::RemoveIndex {
            table: source.into(),
            columns: vec!["private_note".into()],
        }
        .state_forwards(&mut desired)?;
        Op::RemoveColumn {
            table: source.into(),
            column: "private_note".into(),
        }
        .state_forwards(&mut desired)?;
        let select = if qualified_star {
            "SELECT i.* FROM items AS i JOIN details AS d ON i.id = d.id"
        } else {
            add_column(&mut desired, "label")?;
            "SELECT COALESCE(label, id) AS id FROM items"
        };
        desired.add_schema_object(report("item_report", select))?;
        let mut files = baseline(&old)?;
        let (db, schema) = fixture().await;
        db.migrate(graph(&files)?).await?;
        db.execute_sql("INSERT INTO items VALUES(7,42)", vec![])
            .await?;
        db.execute_sql("INSERT INTO details VALUES(7,42)", vec![])
            .await?;
        update(&mut files, &desired, &format!("{source}.private_note"))?;
        install(&db, &files, "reports").await?;
        let g = graph(&files)?;
        let target = &g.app_heads("reports")[0].id;
        let plan = g.plan_forward(&Default::default(), Some(target))?;
        assert!(
            files
                .iter()
                .filter(|f| plan.iter().any(|step| step.id == f.id))
                .all(|f| !f
                    .operations
                    .iter()
                    .any(|op| matches!(op, Op::RemoveIndex { .. } | Op::RemoveColumn { .. })))
        );
        let private = format!("SELECT private_note FROM {source}");
        assert_eq!(value(&db, &private).await?, 42);
        assert_eq!(value(&db, "SELECT id FROM item_report").await?, 7);
        db.migrate(graph(&files)?).await?;
        assert!(db.query_sql(private, vec![]).await.is_err());
        assert_eq!(value(&db, "SELECT id FROM item_report").await?, 7);
        cleanup(schema).await?;
    }
    Ok(())
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn second_report_release_preserves_an_unapplied_archive_retirement() -> Result<()> {
    let mut desired = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
    let mut old = desired.clone();
    old.add_schema_object(table("stock", "archive"))?;
    let mut files = baseline(&old)?;
    let (db, schema) = fixture().await;
    db.migrate(graph(&files)?).await?;
    db.execute_sql("INSERT INTO items VALUES (7)", vec![])
        .await?;
    db.execute_sql("INSERT INTO archive VALUES (42)", vec![])
        .await?;
    for (field, name) in [("label", "first_report"), ("score", "second_report")] {
        add_column(&mut desired, field)?;
        desired.add_schema_object(report(
            name,
            &format!("SELECT COALESCE({field}, id) AS id FROM items"),
        ))?;
        update(&mut files, &desired, "archive")?;
        install(&db, &files, "reports").await?;
        assert_eq!(value(&db, "SELECT id FROM archive").await?, 42);
        assert_eq!(value(&db, &format!("SELECT id FROM {name}")).await?, 7);
    }
    db.migrate(graph(&files)?).await?;
    assert!(
        db.query_sql("SELECT id FROM archive", vec![])
            .await
            .is_err()
    );
    for name in ["first_report", "second_report"] {
        assert_eq!(value(&db, &format!("SELECT id FROM {name}")).await?, 7);
    }
    cleanup(schema).await
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn rebuilding_stock_does_not_install_an_absent_report() -> Result<()> {
    let mut desired = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
    let mut old = desired.clone();
    old.add_schema_object(table("reports", "archive"))?;
    let mut files = baseline(&old)?;
    let (db, schema) = fixture().await;
    db.migrate(graph(&files)?).await?;
    db.execute_sql("INSERT INTO archive VALUES(42)", vec![])
        .await?;
    db.execute_sql("INSERT INTO items VALUES(7)", vec![])
        .await?;
    desired.add_schema_object(report("item_report", "SELECT id FROM items"))?;
    update(&mut files, &desired, "archive")?;
    Op::AddIndex {
        table: "items".into(),
        columns: vec!["id".into()],
    }
    .state_forwards(&mut desired)?;
    update(&mut files, &desired, "")?;
    install(&db, &files, "stock").await?;
    assert_eq!(value(&db, "SELECT id FROM archive").await?, 42);
    assert!(
        db.query_sql("SELECT id FROM item_report", vec![])
            .await
            .is_err()
    );
    db.migrate(graph(&files)?).await?;
    assert!(
        db.query_sql("SELECT id FROM archive", vec![])
            .await
            .is_err()
    );
    assert_eq!(value(&db, "SELECT id FROM item_report").await?, 7);
    cleanup(schema).await
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn scoped_and_global_files_preserve_the_same_transitive_provider_rows() -> Result<()> {
    let base = baseline(&ProjectState::from_app_schema(
        "stock",
        &SchemaSet::new().model::<LegacyB>(),
    ))?;
    let mut desired = ProjectState::from_app_schema(
        "catalog",
        &SchemaSet::new().model::<CatalogA>().model::<CatalogB>(),
    );
    for name in ["parent_a", "parent_b"] {
        desired.add_schema_object(table("stock", name))?;
    }
    desired.add_schema_object(report("item_report", "SELECT id FROM catalog_a"))?;
    for scoped in [false, true] {
        let mut files = base.clone();
        if scoped {
            let policy = GenerationPolicy {
                allow_drop: ["parent_b".into()].into(),
                ..Default::default()
            };
            for app in ["stock", "catalog", "reports"] {
                files.extend(graph(&files)?.generate_for_app(&desired, app, "update", &policy)?);
            }
        } else {
            update(&mut files, &desired, "parent_b")?;
        }
        let (db, schema) = fixture().await;
        db.migrate(graph(&base)?).await?;
        db.execute_sql("INSERT INTO parent_b VALUES (42)", vec![])
            .await?;
        install(&db, &files, "reports").await?;
        assert_eq!(value(&db, "SELECT id FROM parent_b").await?, 42);
        db.execute_sql("INSERT INTO catalog_a(id) VALUES (7)", vec![])
            .await?;
        assert_eq!(value(&db, "SELECT id FROM item_report").await?, 7);
        db.migrate(graph(&files)?).await?;
        assert_eq!(value(&db, "SELECT count(*) FROM parent_b").await?, 0);
        assert_eq!(value(&db, "SELECT id FROM item_report").await?, 7);
        cleanup(schema).await?;
    }
    Ok(())
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn appending_a_column_refreshes_only_installed_star_views() -> Result<()> {
    let mut desired = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
    let mut old = desired.clone();
    old.add_schema_object(table("reports", "archive"))?;
    let initial = baseline(&old)?;
    desired.add_schema_object(report("item_report", "SELECT * FROM items"))?;
    desired.add_schema_object(report("report_copy", "SELECT * FROM item_report"))?;
    let mut first = initial.clone();
    update(&mut first, &desired, "archive")?;
    add_column(&mut desired, "label")?;
    let mut files = first.clone();
    update(&mut files, &desired, "")?;
    for installed in [false, true] {
        let (db, schema) = fixture().await;
        db.migrate(graph(&initial)?).await?;
        db.execute_sql("INSERT INTO items VALUES (7)", vec![])
            .await?;
        db.execute_sql("INSERT INTO archive VALUES (42)", vec![])
            .await?;
        if installed {
            install(&db, &first, "reports").await?;
        }
        install(&db, &files, "stock").await?;
        db.execute_sql("UPDATE items SET label=9", vec![]).await?;
        if installed {
            assert_eq!(value(&db, "SELECT label FROM report_copy").await?, 9);
        } else {
            assert_eq!(value(&db, "SELECT id FROM archive").await?, 42);
            assert!(
                db.query_sql("SELECT id FROM item_report", vec![])
                    .await
                    .is_err()
            );
        }
        db.migrate(graph(&files)?).await?;
        assert_eq!(value(&db, "SELECT label FROM report_copy").await?, 9);
        assert!(
            db.query_sql("SELECT id FROM archive", vec![])
                .await
                .is_err()
        );
        cleanup(schema).await?;
    }
    Ok(())
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn report_alias_hides_the_original_table_name() -> Result<()> {
    let mut desired =
        ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>().model::<Detail>());
    add_column(&mut desired, "note")?;
    let initial = baseline(&desired)?;
    Op::RemoveColumn {
        table: "items".into(),
        column: "note".into(),
    }
    .state_forwards(&mut desired)?;
    desired.add_schema_object(report("item_report",
        "SELECT items.* FROM items AS i JOIN details items ON items.id = i.id WHERE EXISTS (SELECT 1 FROM items AS items WHERE items.id = i.id)"))?;
    let mut files = initial.clone();
    update(&mut files, &desired, "items.note")?;
    let (db, schema) = fixture().await;
    db.migrate(graph(&initial)?).await?;
    db.execute_sql("INSERT INTO items VALUES (7,42)", vec![])
        .await?;
    db.execute_sql("INSERT INTO details VALUES (7,9)", vec![])
        .await?;
    install(&db, &files, "reports").await?;
    assert_eq!(value(&db, "SELECT private_note FROM item_report").await?, 9);
    assert_eq!(value(&db, "SELECT note FROM items").await?, 42);
    db.migrate(graph(&files)?).await?;
    assert!(
        db.query_sql("SELECT note FROM items", vec![])
            .await
            .is_err()
    );
    assert_eq!(value(&db, "SELECT private_note FROM item_report").await?, 9);
    cleanup(schema).await
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn checks_on_new_sibling_columns_are_installed_after_their_inputs() -> Result<()> {
    let mut desired = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
    let initial = baseline(&desired)?;
    for (name, check) in [
        ("amount", "amount <= ceiling"),
        ("ceiling", "ceiling <= amount + 10"),
    ] {
        add_column(&mut desired, name)?;
        let old_column = desired.tables()[0].columns.last().unwrap().clone();
        let mut new_column = old_column.clone();
        new_column.check = Some(check.into());
        Op::AlterColumn {
            table: "items".into(),
            old_column,
            new_column,
        }
        .state_forwards(&mut desired)?;
    }
    let mut files = initial.clone();
    update(&mut files, &desired, "")?;
    let (db, schema) = fixture().await;
    db.migrate(graph(&initial)?).await?;
    db.execute_sql("INSERT INTO items VALUES(7)", vec![])
        .await?;
    install(&db, &files, "stock").await?;
    db.execute_sql("INSERT INTO items VALUES(8, 3, 4)", vec![])
        .await?;
    assert!(
        db.execute_sql("INSERT INTO items VALUES(9, 5, 4)", vec![])
            .await
            .is_err()
    );
    assert!(
        db.execute_sql("INSERT INTO items VALUES(9, 1, 12)", vec![])
            .await
            .is_err()
    );
    assert_eq!(value(&db, "SELECT count(*) FROM items").await?, 2);
    cleanup(schema).await
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn scoped_generation_only_requires_relevant_manual_sql_table_edits() -> Result<()> {
    let mut old = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
    let audit = table("stock", "raw_audit");
    old.add_schema_object(audit.clone())?;
    old.add_schema_object(report("retiring_report", "SELECT id FROM raw_audit"))?;
    let initial = baseline(&old)?;
    let original = graph(&initial)?;
    let mut desired = old.clone();
    Op::DropSchemaObject {
        kind: audit.kind,
        name: audit.name.clone(),
    }
    .state_forwards(&mut desired)?;
    desired.add_schema_object(SchemaObjectState {
        sql: "CREATE TABLE raw_audit(id BIGINT PRIMARY KEY, note TEXT)".into(),
        ..audit
    })?;
    Op::DropSchemaObject {
        kind: SchemaObjectKind::View,
        name: "retiring_report".into(),
    }
    .state_forwards(&mut desired)?;
    desired.add_schema_object(report("item_report", "SELECT id FROM items"))?;
    let files =
        original.generate_for_app(&desired, "reports", "independent", &Default::default())?;
    assert!(files.iter().flat_map(|f| &f.operations).all(|op| matches!(
        op,
        Op::DropSchemaObject {
            kind: SchemaObjectKind::View,
            ..
        } | Op::CreateSchemaObject { .. }
    )));
    assert!(
        original
            .generate(&desired, MigrationLayout::PerApp, "all")
            .unwrap_err()
            .to_string()
            .contains("raw_audit")
    );
    desired.add_schema_object(report("audit_report", "SELECT note FROM raw_audit"))?;
    assert!(
        original
            .generate_for_app(&desired, "reports", "dependent", &Default::default())
            .unwrap_err()
            .to_string()
            .contains("raw_audit")
    );
    let (db, schema) = fixture().await;
    db.migrate(original).await?;
    db.execute_sql("INSERT INTO items VALUES (7)", vec![])
        .await?;
    db.execute_sql("INSERT INTO raw_audit VALUES (42)", vec![])
        .await?;
    install(&db, &[initial, files].concat(), "reports").await?;
    assert_eq!(value(&db, "SELECT id FROM item_report").await?, 7);
    assert_eq!(value(&db, "SELECT id FROM raw_audit").await?, 42);
    assert!(
        db.query_sql("SELECT id FROM retiring_report", vec![])
            .await
            .is_err()
    );
    cleanup(schema).await
}

#[cfg(feature = "sqlite")]
#[test]
fn sql_preview_and_executor_restore_commented_views_and_triggers() -> Result<()> {
    use che_orm::migration::sqlite_executor::apply_operation_migrations_on_connection as apply;
    let mut desired = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
    for (kind, name, sql) in [
        (
            SchemaObjectKind::View,
            "first_report",
            "CREATE VIEW first_report AS SELECT id, '; --literal' AS label FROM items -- reports v1",
        ),
        (
            SchemaObjectKind::View,
            "outer_report",
            "CREATE VIEW outer_report AS SELECT * FROM first_report; /* trailing */",
        ),
        (
            SchemaObjectKind::Trigger,
            "positive_items",
            "CREATE TRIGGER positive_items BEFORE INSERT ON items WHEN NEW.id = 9 BEGIN SELECT RAISE(ABORT,'blocked; by trigger'); END; -- after END",
        ),
    ] {
        desired.add_schema_object(SchemaObjectState {
            app: "stock".into(),
            kind,
            name: name.into(),
            sql: sql.into(),
        })?;
    }
    let initial = baseline(&desired)?;
    let mut files = initial.clone();
    Op::AddIndex {
        table: "items".into(),
        columns: vec!["id".into()],
    }
    .state_forwards(&mut desired)?;
    update(&mut files, &desired, "")?;
    let full = graph(&files)?;
    let sql = full.migration_sql_script(&full.app_heads("stock")[0].id)?;
    for use_preview in [true, false] {
        let mut db = che_orm::rusqlite::Connection::open_in_memory()?;
        apply(&mut db, graph(&initial)?)?;
        db.execute("INSERT INTO items VALUES (7)", [])?;
        if use_preview {
            db.execute_batch(&sql)?;
        } else {
            apply(&mut db, full.clone())?;
        }
        assert_eq!(
            db.query_row("SELECT id, label FROM outer_report", [], |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?
            )))?,
            (7, "; --literal".into())
        );
        assert!(
            db.execute("INSERT INTO items VALUES (9)", [])
                .unwrap_err()
                .to_string()
                .contains("blocked; by trigger")
        );
    }
    Ok(())
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL"]
async fn postgres_refresh_preserves_view_metadata_and_handles_reordered_outputs() -> Result<()> {
    for select in ["SELECT * FROM items", "SELECT i.*, 0 AS tail FROM items i"] {
        let mut desired = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
        desired.add_schema_object(report("item_report", select))?;
        let mut files = baseline(&desired)?;
        let (db, schema) = fixture().await;
        db.migrate(graph(&files)?).await?;
        db.execute_sql("INSERT INTO items VALUES (7)", vec![])
            .await?;
        db.execute_sql("COMMENT ON VIEW item_report IS 'installed report'", vec![])
            .await?;
        add_column(&mut desired, "label")?;
        update(&mut files, &desired, "")?;
        if select.contains("tail") {
            assert!(db.migrate(graph(&files)?).await.is_err());
            assert_eq!(value(&db, "SELECT id FROM item_report").await?, 7);
            assert!(
                db.query_sql("SELECT label FROM items", vec![])
                    .await
                    .is_err()
            );
            db.execute_sql("COMMENT ON VIEW item_report IS NULL", vec![])
                .await?;
        }
        db.migrate(graph(&files)?).await?;
        db.execute_sql("UPDATE items SET label=42", vec![]).await?;
        assert_eq!(value(&db, "SELECT label FROM item_report").await?, 42);
        if select.contains("tail") {
            assert_eq!(value(&db, "SELECT tail::bigint FROM item_report").await?, 0);
        } else {
            assert_eq!(value(&db, "SELECT count(*) WHERE obj_description('item_report'::regclass) = 'installed report'").await?, 1);
        }
        cleanup(schema).await?;
    }
    Ok(())
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn changing_a_default_preserves_the_absent_report_and_its_archive() -> Result<()> {
    for projection in ["*", "label"] {
        let mut desired = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
        add_column(&mut desired, "label")?;
        let mut old = desired.clone();
        old.add_schema_object(table("reports", "archive"))?;
        let mut files = baseline(&old)?;
        let (db, schema) = fixture().await;
        db.migrate(graph(&files)?).await?;
        db.execute_sql("INSERT INTO archive VALUES (42)", vec![])
            .await?;
        desired.add_schema_object(report(
            "item_report",
            &format!("SELECT {projection} FROM items"),
        ))?;
        update(&mut files, &desired, "archive")?;
        alter_column(&mut desired, "label", |col| col.default = Some("9".into()))?;
        update(&mut files, &desired, "")?;
        install(&db, &files, "stock").await?;
        assert_eq!(value(&db, "SELECT id FROM archive").await?, 42);
        assert!(
            db.query_sql("SELECT label FROM item_report", vec![])
                .await
                .is_err()
        );
        db.execute_sql("INSERT INTO items(id) VALUES (7)", vec![])
            .await?;
        assert_eq!(value(&db, "SELECT label FROM items").await?, 9);
        db.migrate(graph(&files)?).await?;
        assert!(
            db.query_sql("SELECT id FROM archive", vec![])
                .await
                .is_err()
        );
        assert_eq!(value(&db, "SELECT label FROM item_report").await?, 9);
        cleanup(schema).await?;
    }
    Ok(())
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn derived_report_reads_only_its_projected_columns() -> Result<()> {
    for select in [
        "SELECT d.id FROM (SELECT * FROM details) AS d",
        "WITH d AS (SELECT * FROM details) SELECT id FROM d",
        "WITH d AS (SELECT * FROM details), e AS (SELECT d.* FROM d) SELECT e.id FROM e",
        "SELECT e.id FROM (SELECT d.* FROM (SELECT * FROM details) d) AS e",
        "SELECT d.* FROM (SELECT id FROM items) AS d JOIN details AS x ON x.id=d.id",
        "SELECT d.* FROM (SELECT * FROM items) AS d JOIN details AS x ON x.id=d.id",
        "WITH d AS (SELECT id FROM items) SELECT d.* FROM d JOIN details AS x ON x.id=d.id",
    ] {
        let mut desired = ProjectState::from_app_schema(
            "stock",
            &SchemaSet::new().model::<Item>().model::<Detail>(),
        );
        let mut files = baseline(&desired)?;
        let (db, schema) = fixture().await;
        db.migrate(graph(&files)?).await?;
        db.execute_sql("INSERT INTO details VALUES(7,42)", vec![])
            .await?;
        db.execute_sql("INSERT INTO items VALUES(7)", vec![])
            .await?;
        Op::RemoveColumn {
            table: "details".into(),
            column: "private_note".into(),
        }
        .state_forwards(&mut desired)?;
        desired.add_schema_object(report("item_report", select))?;
        update(&mut files, &desired, "details.private_note")?;
        install(&db, &files, "reports").await?;
        assert_eq!(value(&db, "SELECT private_note FROM details").await?, 42);
        assert_eq!(value(&db, "SELECT id FROM item_report").await?, 7);
        db.migrate(graph(&files)?).await?;
        assert!(
            db.query_sql("SELECT private_note FROM details", vec![])
                .await
                .is_err()
        );
        assert_eq!(value(&db, "SELECT id FROM item_report").await?, 7);
        cleanup(schema).await?;
    }
    Ok(())
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn removing_a_check_input_releases_the_old_check_first() -> Result<()> {
    let mut desired = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
    for name in ["amount", "ceiling"] {
        add_column(&mut desired, name)?;
    }
    alter_column(&mut desired, "amount", |c| {
        c.check = Some("amount <= ceiling".into())
    })?;
    let mut files = baseline(&desired)?;
    let (db, schema) = fixture().await;
    db.migrate(graph(&files)?).await?;
    db.execute_sql("INSERT INTO items VALUES(1,42,50)", vec![])
        .await?;
    alter_column(&mut desired, "amount", |c| {
        c.check = Some("amount >= 0".into())
    })?;
    Op::RemoveColumn {
        table: "items".into(),
        column: "ceiling".into(),
    }
    .state_forwards(&mut desired)?;
    update(&mut files, &desired, "items.ceiling")?;
    install(&db, &files, "stock").await?;
    assert_eq!(value(&db, "SELECT amount FROM items").await?, 42);
    assert!(
        db.query_sql("SELECT ceiling FROM items", vec![])
            .await
            .is_err()
    );
    assert!(
        db.execute_sql("UPDATE items SET amount=-1", vec![])
            .await
            .is_err()
    );
    cleanup(schema).await
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL"]
async fn table_query_installs_its_source_and_refreshes_the_report() -> Result<()> {
    let mut desired = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
    let mut files = baseline(&desired)?;
    desired.add_schema_object(report("item_report", "TABLE items"))?;
    update(&mut files, &desired, "")?;
    let (db, schema) = fixture().await;
    install(&db, &files, "reports").await?;
    db.execute_sql("INSERT INTO items VALUES(7)", vec![])
        .await?;
    assert_eq!(value(&db, "SELECT id FROM item_report").await?, 7);
    add_column(&mut desired, "label")?;
    update(&mut files, &desired, "")?;
    install(&db, &files, "stock").await?;
    db.execute_sql("UPDATE items SET label=42", vec![]).await?;
    assert_eq!(value(&db, "SELECT label FROM item_report").await?, 42);
    cleanup(schema).await
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL"]
async fn recreating_a_view_restores_its_trigger_comment() -> Result<()> {
    let mut desired = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
    desired.add_schema_object(report("item_report", "SELECT i.*, 0 AS tail FROM items i"))?;
    desired.add_schema_object(SchemaObjectState {
        app: "reports".into(),
        kind: SchemaObjectKind::Trigger,
        name: "report_insert".into(),
        sql: "CREATE TRIGGER report_insert INSTEAD OF INSERT ON item_report
              FOR EACH ROW EXECUTE FUNCTION write_item()"
            .into(),
    })?;
    let mut files = baseline(&desired)?;
    let (db, schema) = fixture().await;
    db.execute_sql(
        "CREATE FUNCTION write_item() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN INSERT INTO items(id) VALUES(NEW.id); RETURN NEW; END $$",
        vec![],
    )
    .await?;
    db.migrate(graph(&files)?).await?;
    let comment = "report's insert; persists";
    db.execute_sql(
        "COMMENT ON TRIGGER report_insert ON item_report IS 'report''s insert; persists'",
        vec![],
    )
    .await?;
    add_column(&mut desired, "label")?;
    update(&mut files, &desired, "")?;
    install(&db, &files, "stock").await?;
    let rows = db
        .query_sql(
            "SELECT pg_catalog.obj_description(oid,'pg_trigger') FROM pg_catalog.pg_trigger
         WHERE tgname='report_insert' AND tgrelid='item_report'::regclass",
            vec![],
        )
        .await?;
    assert_eq!(rows[0].get::<String>(0)?, comment);
    db.execute_sql("INSERT INTO item_report(id) VALUES(42)", vec![])
        .await?;
    assert_eq!(value(&db, "SELECT id FROM items").await?, 42);
    cleanup(schema).await
}

#[test]
fn unresolved_star_requires_review_before_a_column_removal() -> Result<()> {
    let mut desired = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Detail>());
    let files = baseline(&desired)?;
    Op::RemoveColumn {
        table: "details".into(),
        column: "private_note".into(),
    }
    .state_forwards(&mut desired)?;
    #[cfg(feature = "sqlite")]
    let query = "SELECT f.* FROM json_each('[7]') AS f JOIN details AS x ON x.id=f.value";
    #[cfg(feature = "postgres")]
    let query = "SELECT f.* FROM generate_series(7,7) AS f JOIN details AS x ON x.id=f";
    desired.add_schema_object(report("item_report", query))?;
    let policy = GenerationPolicy {
        allow_drop: ["details.private_note".into()].into(),
        ..Default::default()
    };
    let err = graph(&files)?
        .generate_with_policy(&desired, MigrationLayout::PerApp, "reports", &policy)
        .unwrap_err();
    assert!(
        matches!(err, che_orm::MigrationError::NeedsManualMigration { reason, .. } if reason.contains("details"))
    );
    desired.add_schema_object(table("logbook", "cache"))?;
    let independent = graph(&files)?.generate_for_app(&desired, "logbook", "initial", &policy)?;
    assert_eq!(independent.len(), 1);
    assert!(independent[0].dependencies.is_empty());
    Ok(())
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn target_installs_provider_of_auto_now_default() -> Result<()> {
    #[derive(Debug, Model)]
    #[orm(table = "items")]
    struct StampedItem {
        #[orm(primary_key)]
        id: i64,
        stamp: time::OffsetDateTime,
    }

    let mut desired =
        ProjectState::from_app_schema("stock", &SchemaSet::new().model::<StampedItem>());
    desired.add_schema_object(table("reports", "input_ids"))?;
    let mut files = baseline(&desired)?;
    alter_column(&mut desired, "stamp", |c| c.auto_now = true)?;
    update(&mut files, &desired, "")?;
    #[cfg(feature = "sqlite")]
    desired.add_schema_object(SchemaObjectState {
        app: "reports".into(),
        kind: SchemaObjectKind::Trigger,
        name: "writer".into(),
        sql: "CREATE TRIGGER writer AFTER INSERT ON input_ids BEGIN INSERT INTO items(id) VALUES(NEW.id); END".into(),
    })?;
    #[cfg(feature = "postgres")]
    desired.add_declared_object(
        report("stamped_items", "SELECT id FROM items"),
        [Req::Default("items".into(), "stamp".into())],
    )?;
    update(&mut files, &desired, "")?;

    let (db, schema) = fixture().await;
    install(&db, &files, "reports").await?;
    #[cfg(feature = "sqlite")]
    db.execute_sql("INSERT INTO input_ids VALUES(7)", vec![])
        .await?;
    #[cfg(feature = "postgres")]
    db.execute_sql("INSERT INTO items(id) VALUES(7)", vec![])
        .await?;
    assert_eq!(
        value(&db, "SELECT id FROM items WHERE stamp IS NOT NULL").await?,
        7
    );
    cleanup(schema).await
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn adding_required_unique_column_with_default_requires_staged_backfill() -> Result<()> {
    let mut desired = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
    let mut files = baseline(&desired)?;
    let (db, schema) = fixture().await;
    db.migrate(graph(&files)?).await?;
    db.execute_sql("INSERT INTO items VALUES(7),(8)", vec![])
        .await?;

    add_column(&mut desired, "code")?;
    let nullable = desired.clone();
    alter_column(&mut desired, "code", |c| {
        c.nullable = false;
        c.unique = true;
        c.default = Some("42".into());
    })?;
    assert!(matches!(
        graph(&files)?
            .generate(&desired, MigrationLayout::PerApp, "add_code")
            .unwrap_err(),
        che_orm::MigrationError::NeedsManualMigration { resource, reason }
            if resource == "items.code" && reason.contains("backfill distinct values")
    ));
    assert_eq!(baseline(&desired)?.len(), 1);

    update(&mut files, &nullable, "")?;
    let mut backfill = graph(&files)?.manual_migration("stock", "backfill_code", false)?;
    backfill = Migration::new(
        backfill.id,
        backfill.dependencies,
        vec![Op::BackfillColumn {
            table: "items".into(),
            column: "code".into(),
            value: "id".into(),
        }],
    );
    files.push(backfill);
    alter_column(&mut desired, "code", |c| c.default = None)?;
    update(&mut files, &desired, "")?;
    db.migrate(graph(&files)?).await?;
    assert_eq!(
        value(&db, "SELECT count(*) FROM items WHERE code = id").await?,
        2
    );
    assert!(
        db.execute_sql("INSERT INTO items VALUES(9,7)", vec![])
            .await
            .is_err()
    );
    assert!(
        db.execute_sql("INSERT INTO items(id) VALUES(9)", vec![])
            .await
            .is_err()
    );
    cleanup(schema).await
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn installing_a_writer_uses_the_required_defaults_and_checks() -> Result<()> {
    let mut desired = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
    add_column(&mut desired, "label")?;
    add_column(&mut desired, "amount")?;
    alter_column(&mut desired, "amount", |c| {
        c.check = Some("amount > 0".into())
    })?;
    desired.add_schema_object(table("reports", "input_ids"))?;
    let mut files = baseline(&desired)?;
    let (db, schema) = fixture().await;
    db.migrate(graph(&files)?).await?;
    alter_column(&mut desired, "label", |c| c.default = Some("42".into()))?;
    let writer = SchemaObjectState {
        app: "reports".into(), kind: SchemaObjectKind::Trigger, name: "copy_id".into(),
        sql: "CREATE TRIGGER copy_id AFTER INSERT ON input_ids BEGIN INSERT INTO items(id,amount) VALUES(NEW.id,0); END".into(),
    };
    let mut with_writer = desired.clone();
    with_writer.add_schema_object(writer.clone())?;
    let err = graph(&files)?
        .generate_for_app(
            &with_writer,
            "reports",
            "writer",
            &GenerationPolicy::default(),
        )
        .unwrap_err();
    assert!(err.to_string().contains("items.label"));
    update(&mut files, &desired, "")?;
    alter_column(&mut desired, "amount", |c| c.check = None)?;
    desired.add_schema_object(writer)?;
    let err = graph(&files)?
        .generate_for_app(&desired, "reports", "writer", &GenerationPolicy::default())
        .unwrap_err();
    assert!(err.to_string().contains("items.amount"));
    update(&mut files, &desired, "")?;
    install(&db, &files, "reports").await?;
    db.execute_sql("INSERT INTO input_ids VALUES(7)", vec![])
        .await?;
    assert_eq!(value(&db, "SELECT label FROM items WHERE id=7").await?, 42);
    cleanup(schema).await
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn report_installation_depends_on_the_exported_columns_it_uses() -> Result<()> {
    for (query, requires_label) in [
        ("SELECT id FROM all_details", false),
        (
            "SELECT i.id FROM items i WHERE EXISTS (SELECT * FROM details d WHERE d.id=i.id)",
            false,
        ),
        ("SELECT COALESCE(label,id) AS id FROM items", true),
        (
            "WITH d AS (SELECT * FROM items), e AS (SELECT d.* FROM d) SELECT COALESCE(e.label,e.id) AS id FROM e",
            true,
        ),
    ] {
        let mut state = ProjectState::from_app_schema(
            "stock",
            &SchemaSet::new().model::<Item>().model::<Detail>(),
        );
        let mut provider = report("all_details", "SELECT * FROM details");
        provider.app = "stock".into();
        state.add_schema_object(provider)?;
        let base = baseline(&state)?;
        let mut files = base.clone();
        add_column(&mut state, "label")?;
        Op::RemoveColumn {
            table: "details".into(),
            column: "private_note".into(),
        }
        .state_forwards(&mut state)?;
        state.add_schema_object(report("summary", query))?;
        update(&mut files, &state, "details.private_note")?;
        for targeted in [true, false] {
            let (db, schema) = fixture().await;
            db.migrate(graph(&base)?).await?;
            db.execute_sql("INSERT INTO items(id) VALUES(7)", vec![])
                .await?;
            db.execute_sql("INSERT INTO details VALUES(7,42)", vec![])
                .await?;
            if targeted {
                install(&db, &files, "reports").await?;
                assert_eq!(value(&db, "SELECT private_note FROM details").await?, 42);
                assert_eq!(value(&db, "SELECT id FROM summary").await?, 7);
                assert_eq!(
                    db.query_sql("SELECT label FROM items", vec![])
                        .await
                        .is_ok(),
                    requires_label
                );
            }
            db.migrate(graph(&files)?).await?;
            assert_eq!(value(&db, "SELECT id FROM summary").await?, 7);
            assert!(
                db.query_sql("SELECT private_note FROM details", vec![])
                    .await
                    .is_err()
            );
            cleanup(schema).await?;
        }
    }
    Ok(())
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn forwarded_wildcard_requires_the_new_output_shape() -> Result<()> {
    let mut state = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Detail>());
    let mut files = baseline(&state)?;
    let (db, schema) = fixture().await;
    db.migrate(graph(&files)?).await?;
    db.execute_sql("INSERT INTO details VALUES(7,42)", vec![])
        .await?;
    Op::RemoveColumn {
        table: "details".into(),
        column: "private_note".into(),
    }
    .state_forwards(&mut state)?;
    state.add_schema_object(report(
        "summary",
        "WITH d AS (SELECT * FROM details) SELECT d.* FROM d",
    ))?;
    update(&mut files, &state, "details.private_note")?;
    install(&db, &files, "reports").await?;
    assert_eq!(value(&db, "SELECT id FROM summary").await?, 7);
    assert!(
        db.query_sql("SELECT private_note FROM summary", vec![])
            .await
            .is_err()
    );
    assert_eq!(db.migrate(graph(&files)?).await?, 0);
    cleanup(schema).await
}

fn drop_object(state: &mut ProjectState, kind: SchemaObjectKind, name: &str) -> Result<()> {
    Op::DropSchemaObject {
        kind,
        name: name.into(),
    }
    .state_forwards(state)?;
    Ok(())
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn editing_stock_leaves_the_reports_release_pending() -> Result<()> {
    for retired_star in [true, false] {
        let mut state = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
        state.add_schema_object(table("reports", "archive"))?;
        if retired_star {
            state.add_schema_object(report("all_items", "SELECT * FROM items"))?;
        } else {
            add_column(&mut state, "note")?;
        }
        let mut files = baseline(&state)?;
        let (db, schema) = fixture().await;
        db.migrate(graph(&files)?).await?;
        db.execute_sql("INSERT INTO archive VALUES(42)", vec![])
            .await?;
        db.execute_sql("INSERT INTO items(id) VALUES(7)", vec![])
            .await?;
        drop_object(&mut state, SchemaObjectKind::Table, "archive")?;
        if retired_star {
            drop_object(&mut state, SchemaObjectKind::View, "all_items")?;
        } else {
            state.add_schema_object(report(
                "sorted",
                "SELECT i.id AS note FROM items AS i ORDER BY note",
            ))?;
        }
        update(&mut files, &state, "archive")?;
        if retired_star {
            add_column(&mut state, "note")?;
        } else {
            Op::RemoveColumn {
                table: "items".into(),
                column: "note".into(),
            }
            .state_forwards(&mut state)?;
        }
        update(
            &mut files,
            &state,
            if retired_star { "" } else { "items.note" },
        )?;
        install(&db, &files, "stock").await?;
        assert_eq!(value(&db, "SELECT id FROM archive").await?, 42);
        if retired_star {
            assert_eq!(value(&db, "SELECT id FROM all_items").await?, 7);
        }
        db.migrate(graph(&files)?).await?;
        assert!(
            db.query_sql("SELECT id FROM archive", vec![])
                .await
                .is_err()
        );
        if !retired_star {
            assert_eq!(value(&db, "SELECT note FROM sorted").await?, 7);
        }
        cleanup(schema).await?;
    }
    Ok(())
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn writers_depend_on_the_defaults_and_constraints_they_observe() -> Result<()> {
    for positional in [true, false] {
        let mut state = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
        add_column(&mut state, "label")?;
        add_column(&mut state, "note")?;
        state.add_schema_object(table("reports", "input_ids"))?;
        state.add_schema_object(table("stock", "archive"))?;
        let mut files = baseline(&state)?;
        let (db, schema) = fixture().await;
        db.migrate(graph(&files)?).await?;
        db.execute_sql("INSERT INTO archive VALUES(42)", vec![])
            .await?;
        drop_object(&mut state, SchemaObjectKind::Table, "archive")?;
        let sql = if positional {
            alter_column(&mut state, "label", |c| c.default = Some("9".into()))?;
            "INSERT INTO items VALUES(NEW.id,17,0)"
        } else {
            alter_column(&mut state, "note", |c| c.unique = true)?;
            db.execute_sql("INSERT INTO items VALUES(7,0,3)", vec![])
                .await?;
            "UPDATE items SET label=17 WHERE id=NEW.id"
        };
        update(&mut files, &state, "archive")?;
        state.add_schema_object(SchemaObjectState {
            app: "reports".into(),
            kind: SchemaObjectKind::Trigger,
            name: "writer".into(),
            sql: format!("CREATE TRIGGER writer AFTER INSERT ON input_ids BEGIN {sql}; END"),
        })?;
        update(&mut files, &state, "")?;
        install(&db, &files, "reports").await?;
        assert_eq!(value(&db, "SELECT id FROM archive").await?, 42);
        db.execute_sql("INSERT INTO input_ids VALUES(7)", vec![])
            .await?;
        assert_eq!(value(&db, "SELECT label FROM items").await?, 17);
        db.migrate(graph(&files)?).await?;
        assert!(
            db.query_sql("SELECT id FROM archive", vec![])
                .await
                .is_err()
        );
        cleanup(schema).await?;
    }
    Ok(())
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn backfilling_stock_does_not_supply_a_reports_table() -> Result<()> {
    let mut state = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
    add_column(&mut state, "label")?;
    state.add_schema_object(table("stock", "archive"))?;
    let mut files = baseline(&state)?;
    let (db, schema) = fixture().await;
    db.migrate(graph(&files)?).await?;
    db.execute_sql("INSERT INTO items(id) VALUES(7)", vec![])
        .await?;
    db.execute_sql("INSERT INTO archive VALUES(42)", vec![])
        .await?;
    let retire = Migration::new(
        che_orm::MigrationId {
            app: "stock".into(),
            name: "0002_retire".into(),
        },
        vec![graph(&files)?.app_heads("stock")[0].id.clone()],
        vec![
            Op::BackfillColumn {
                table: "items".into(),
                column: "label".into(),
                value: "9".into(),
            },
            Op::DropSchemaObject {
                kind: SchemaObjectKind::Table,
                name: "archive".into(),
            },
        ],
    );
    let backfill = retire.id.clone();
    files.push(retire);
    drop_object(&mut state, SchemaObjectKind::Table, "archive")?;
    state.add_schema_object(table("reports", "cache"))?;
    let mut snapshot = table("snapshots", "labels");
    snapshot.sql = "CREATE TABLE labels AS SELECT label FROM items".into();
    state.add_schema_object(snapshot)?;
    let mut totals = report("totals", "SELECT label FROM items");
    totals.app = "totals".into();
    state.add_declared_object(
        totals,
        [che_orm::SchemaRequirement::Data(
            "items".into(),
            "label".into(),
        )],
    )?;
    update(&mut files, &state, "")?;
    install(&db, &files, "reports").await?;
    assert_eq!(value(&db, "SELECT id FROM archive").await?, 42);
    let g = graph(&files)?;
    for app in ["snapshots", "totals"] {
        let id = &g.app_heads(app)[0].id;
        assert!(
            g.plan_forward(&Default::default(), Some(id))?
                .iter()
                .any(|step| step.id == backfill)
        );
    }
    install(&db, &files, "snapshots").await?;
    assert_eq!(value(&db, "SELECT label FROM labels").await?, 9);
    db.migrate(graph(&files)?).await?;
    assert_eq!(value(&db, "SELECT label FROM items").await?, 9);
    assert!(
        db.query_sql("SELECT id FROM archive", vec![])
            .await
            .is_err()
    );
    cleanup(schema).await
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn explicit_report_inputs_are_frozen_and_limit_targeted_installation() -> Result<()> {
    use che_orm::SchemaRequirement as Req;
    let stock = SchemaSet::new().model::<Detail>();
    let base = baseline(&ProjectState::from_app_schema("stock", &stock))?;
    #[cfg(feature = "postgres")]
    let query = "SELECT f.* FROM generate_series(7,7) AS f JOIN details AS d ON f=d.id";
    #[cfg(feature = "sqlite")]
    let query = "SELECT f.* FROM json_each('[7]') AS f JOIN details AS d ON f.value=d.id";
    let object = report("summary", query);
    let inputs = [Req::Column("details".into(), "id".into())];
    let reports = SchemaSet::new().declared_object(object.clone(), inputs.clone());
    let mut desired = ProjectState::from_app_schemas([("reports", &reports), ("stock", &stock)])?;
    Op::RemoveColumn {
        table: "details".into(),
        column: "private_note".into(),
    }
    .state_forwards(&mut desired)?;
    let mut files = base.clone();
    update(&mut files, &desired, "details.private_note")?;
    let g = graph(&files)?;
    assert_eq!(
        g.replay(Default::default())?.object_requirements(&object),
        Some(&inputs.into())
    );
    let (db, schema) = fixture().await;
    db.migrate(graph(&base)?).await?;
    db.execute_sql("INSERT INTO details VALUES(7,42)", vec![])
        .await?;
    install(&db, &files, "reports").await?;
    assert_eq!(value(&db, "SELECT private_note FROM details").await?, 42);
    db.migrate(g).await?;
    assert_eq!(value(&db, "SELECT count(*) FROM summary").await?, 1);
    assert!(
        db.query_sql("SELECT private_note FROM details", vec![])
            .await
            .is_err()
    );
    cleanup(schema).await
}

#[test]
fn requirements_on_published_objects_upgrade_without_running_ddl() -> Result<()> {
    use che_orm::SchemaRequirement;
    let object = report("summary", "SELECT id FROM items");
    let mut legacy = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
    legacy.add_schema_object(object.clone())?;
    let mut files = baseline(&legacy)?;
    let mut desired = legacy.clone();
    drop_object(&mut desired, SchemaObjectKind::View, "summary")?;
    desired.add_declared_object(
        object.clone(),
        [SchemaRequirement::Column("items".into(), "id".into())],
    )?;
    for state in [&desired, &legacy] {
        let metadata = graph(&files)?.generate_for_app(
            state,
            "reports",
            "contract",
            &GenerationPolicy::default(),
        )?;
        assert_eq!(metadata.len(), 1);
        assert!(matches!(
            metadata[0].operations.as_slice(),
            [Op::SetSchemaRequirements { .. }]
        ));
        let id = metadata[0].id.clone();
        files.extend(metadata);
        let g = graph(&files)?;
        assert!(g.migration_sql(&id)?.is_empty());
        assert_eq!(
            g.replay(Default::default())?.object_requirements(&object),
            state.object_requirements(&object)
        );
        assert!(
            g.generate(state, MigrationLayout::PerApp, "again")?
                .is_empty()
        );
    }
    Ok(())
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn published_stock_rebuild_restores_a_later_installed_report() -> Result<()> {
    let mut state = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Detail>());
    let mut provider = report("all_details", "SELECT * FROM details");
    provider.app = "stock".into();
    let inputs = [Req::ColumnList("details".into())];
    state.add_declared_object(provider.clone(), inputs.clone())?;
    let mut files = baseline(&state)?;
    let (db, schema) = fixture().await;
    db.migrate(graph(&files)?).await?;
    db.execute_sql("INSERT INTO details VALUES(7,42)", vec![])
        .await?;
    let old_file = Migration::new(
        che_orm::MigrationId {
            app: "stock".into(),
            name: "0002_rebuild".into(),
        },
        vec![graph(&files)?.app_heads("stock")[0].id.clone()],
        vec![
            Op::CreateSchemaObject {
                object: table("stock", "before_edit"),
            },
            Op::DropSchemaObject {
                kind: SchemaObjectKind::View,
                name: provider.name.clone(),
            },
            Op::CreateSchemaObject {
                object: table("stock", "within_edit"),
            },
            Op::RemoveColumn {
                table: "details".into(),
                column: "private_note".into(),
            },
            Op::CreateSchemaObject {
                object: provider.clone(),
            },
            Op::CreateSchemaObject {
                object: table("stock", "after_edit"),
            },
            Op::SetSchemaRequirements {
                kind: provider.kind,
                name: provider.name,
                requirements: Some(inputs.into()),
            },
        ],
    );
    files.push(old_file);
    state = graph(&files)?.replay(Default::default())?;
    state.add_schema_object(report("summary", "SELECT id FROM all_details"))?;
    state.add_declared_object(
        report("declared_summary", "SELECT id FROM all_details"),
        [Req::Column("all_details".into(), "id".into())],
    )?;
    update(&mut files, &state, "")?;
    install(&db, &files, "reports").await?;
    assert_eq!(value(&db, "SELECT private_note FROM details").await?, 42);
    db.migrate(graph(&files)?).await?;
    for name in ["before_edit", "within_edit", "after_edit"] {
        assert_eq!(
            value(&db, &format!("SELECT count(*) FROM {name}")).await?,
            0
        );
    }
    assert_eq!(value(&db, "SELECT id FROM summary").await?, 7);
    assert_eq!(value(&db, "SELECT id FROM declared_summary").await?, 7);
    assert!(
        graph(&files)?
            .generate(&state, MigrationLayout::PerApp, "again")?
            .is_empty()
    );
    assert!(
        db.query_sql("SELECT private_note FROM details", vec![])
            .await
            .is_err()
    );
    cleanup(schema).await
}

#[test]
fn an_installed_report_does_not_block_an_independent_scoped_change() -> Result<()> {
    let mut state = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Detail>());
    state.add_schema_object(report("all_details", "SELECT * FROM details"))?;
    let files = baseline(&state)?;
    Op::RemoveColumn {
        table: "details".into(),
        column: "private_note".into(),
    }
    .state_forwards(&mut state)?;
    state.add_schema_object(table("reports", "cache"))?;
    let changes = graph(&files)?.generate_for_app(
        &state,
        "reports",
        "cache",
        &GenerationPolicy::default(),
    )?;
    assert_eq!(changes.len(), 1);
    assert!(
        matches!(changes[0].operations.as_slice(),[Op::CreateSchemaObject{object}] if object.name=="cache")
    );
    Ok(())
}

async fn trigger(
    db: &Database,
    app: &str,
    name: &str,
    target: &str,
    body: &str,
) -> Result<SchemaObjectState> {
    #[cfg(feature = "sqlite")]
    let sql = {
        let _ = db;
        format!("CREATE TRIGGER {name} AFTER INSERT ON {target} BEGIN {body}; END")
    };
    #[cfg(feature = "postgres")]
    let sql = {
        db.execute_sql(format!("CREATE FUNCTION fn_{name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN {body}; RETURN NEW; END $$"), vec![]).await?;
        format!(
            "CREATE TRIGGER {name} AFTER INSERT ON {target} FOR EACH ROW EXECUTE FUNCTION fn_{name}()"
        )
    };
    Ok(SchemaObjectState {
        app: app.into(),
        kind: SchemaObjectKind::Trigger,
        name: name.into(),
        sql,
    })
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn updating_provider_metadata_preserves_archive() -> Result<()> {
    reports_on_original_relation(true).await
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn homonymous_trigger_does_not_version_a_table() -> Result<()> {
    reports_on_original_relation(false).await
}

async fn reports_on_original_relation(metadata: bool) -> Result<()> {
    let mut s = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
    let archive_owner = if metadata { "stock" } else { "audit" };
    s.add_schema_object(table(archive_owner, "archive"))?;
    let mut provider = report("provider", "SELECT id FROM items");
    provider.app = "stock".into();
    if metadata {
        s.add_schema_object(provider.clone())?;
    } else {
        s.add_schema_object(table("audit", "events"))?;
    }
    let mut files = baseline(&s)?;
    let (db, schema) = fixture().await;
    db.migrate(graph(&files)?).await?;
    db.execute_sql("INSERT INTO items VALUES(7)", vec![])
        .await?;
    db.execute_sql("INSERT INTO archive VALUES(42)", vec![])
        .await?;
    drop_object(&mut s, SchemaObjectKind::Table, "archive")?;
    if metadata {
        drop_object(&mut s, SchemaObjectKind::View, "provider")?;
        s.add_declared_object(provider, [Req::Column("items".into(), "id".into())])?;
    } else {
        s.add_schema_object(
            trigger(
                &db,
                "audit",
                "items",
                "events",
                "UPDATE events SET id=id WHERE id=NEW.id",
            )
            .await?,
        )?;
    }
    update(&mut files, &s, "archive")?;
    s.add_schema_object(report(
        "summary",
        if metadata {
            "SELECT id FROM provider"
        } else {
            "SELECT id FROM items"
        },
    ))?;
    update(&mut files, &s, "")?;
    let g = graph(&files)?;
    let steps = g.plan_forward(&Default::default(), Some(&g.app_heads("reports")[0].id))?;
    assert!(
        steps.iter().all(|step| step.id.app != "audit"
            && (step.id.app != "stock" || step.id.name == "0001_initial"))
    );
    install(&db, &files, "reports").await?;
    assert_eq!(value(&db, "SELECT id FROM archive").await?, 42);
    assert_eq!(value(&db, "SELECT id FROM summary").await?, 7);
    db.migrate(g).await?;
    assert!(
        db.query_sql("SELECT id FROM archive", vec![])
            .await
            .is_err()
    );
    assert_eq!(value(&db, "SELECT id FROM summary").await?, 7);
    cleanup(schema).await
}

async fn snapshot_after_backfills(declared_view: bool, alias: bool) -> Result<()> {
    let mut s = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
    add_column(&mut s, "label")?;
    s.add_schema_object(table("archive", "old_rows"))?;
    if declared_view {
        let mut view = report(
            "labels",
            if alias {
                "SELECT *, label AS report_label FROM items"
            } else {
                "SELECT label FROM items"
            },
        );
        view.app = "stock".into();
        let input = if alias {
            Req::ColumnList("items".into())
        } else {
            Req::Column("items".into(), "label".into())
        };
        s.add_declared_object(view, [input])?;
    }
    let mut files = baseline(&s)?;
    let (db, schema) = fixture().await;
    db.migrate(graph(&files)?).await?;
    db.execute_sql("INSERT INTO items(id) VALUES(1),(2)", vec![])
        .await?;
    db.execute_sql("INSERT INTO old_rows VALUES(42)", vec![])
        .await?;
    let parent = graph(&files)?.app_heads("stock")[0].id.clone();
    let fills: &[(&str, &str)] = if declared_view {
        &[("stock", "9")]
    } else {
        &[
            ("east", "CASE WHEN id=1 THEN 17 END"),
            ("west", "CASE WHEN id=2 THEN 29 END"),
        ]
    };
    for &(app, value) in fills {
        files.push(Migration::new(
            che_orm::MigrationId {
                app: app.into(),
                name: "0002_fill".into(),
            },
            vec![parent.clone()],
            vec![Op::BackfillColumn {
                table: "items".into(),
                column: "label".into(),
                value: value.into(),
            }],
        ));
    }
    drop_object(&mut s, SchemaObjectKind::Table, "old_rows")?;
    let mut snapshot = table("reports", "snapshot");
    if declared_view {
        snapshot.sql = if alias {
            "CREATE TABLE snapshot AS SELECT report_label AS label FROM labels"
        } else {
            "CREATE TABLE snapshot AS SELECT label FROM labels"
        }
        .into();
        s.add_schema_object(snapshot)?;
    } else {
        snapshot.sql = "CREATE TABLE snapshot AS SELECT label FROM items".into();
        s.add_declared_object(snapshot, [Req::Data("items".into(), "label".into())])?;
    }
    update(&mut files, &s, "old_rows")?;
    install(&db, &files, "reports").await?;
    assert_eq!(value(&db, "SELECT id FROM old_rows").await?, 42);
    // SUM(bigint) is numeric on PostgreSQL.
    let sum = "SELECT CAST(sum(label) AS BIGINT) FROM snapshot";
    let expected = if declared_view { 18 } else { 46 };
    assert_eq!(value(&db, sum).await?, expected);
    db.migrate(graph(&files)?).await?;
    assert_eq!(value(&db, sum).await?, expected);
    assert!(
        db.query_sql("SELECT id FROM old_rows", vec![])
            .await
            .is_err()
    );
    cleanup(schema).await
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn snapshot_inherits_data_dependencies_through_declared_view() -> Result<()> {
    snapshot_after_backfills(true, false).await?;
    snapshot_after_backfills(true, true).await
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn snapshot_depends_on_each_independent_backfill() -> Result<()> {
    snapshot_after_backfills(false, false).await
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn upsert_requires_the_composite_unique_constraint() -> Result<()> {
    let mut s = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
    add_column(&mut s, "label")?;
    add_column(&mut s, "note")?;
    s.add_schema_object(table("reports", "events"))?;
    s.add_schema_object(table("archive", "old_rows"))?;
    let mut files = baseline(&s)?;
    let (db, schema) = fixture().await;
    db.migrate(graph(&files)?).await?;
    db.execute_sql("INSERT INTO old_rows VALUES(42)", vec![])
        .await?;
    Op::AddUniqueConstraint {
        table: "items".into(),
        columns: vec!["label".into(), "note".into()],
    }
    .state_forwards(&mut s)?;
    update(&mut files, &s, "")?;
    drop_object(&mut s, SchemaObjectKind::Table, "old_rows")?;
    let ingest = trigger(
        &db,
        "reports",
        "ingest",
        "events",
        "INSERT INTO items(label,note) VALUES(7,1) ON CONFLICT(label,note) DO NOTHING",
    )
    .await?;
    #[cfg(feature = "sqlite")]
    s.add_schema_object(ingest)?;
    #[cfg(feature = "postgres")]
    s.add_declared_object(
        ingest,
        [
            Req::Relation("events".into()),
            Req::WriteConstraint("items".into(), "label".into()),
            Req::WriteConstraint("items".into(), "note".into()),
        ],
    )?;
    update(&mut files, &s, "old_rows")?;
    install(&db, &files, "reports").await?;
    db.execute_sql("INSERT INTO events VALUES(1),(2)", vec![])
        .await?;
    assert_eq!(value(&db, "SELECT count(*) FROM items").await?, 1);
    assert_eq!(value(&db, "SELECT id FROM old_rows").await?, 42);
    db.migrate(graph(&files)?).await?;
    db.execute_sql("INSERT INTO events VALUES(3)", vec![])
        .await?;
    assert_eq!(value(&db, "SELECT count(*) FROM items").await?, 1);
    assert!(
        db.query_sql("SELECT id FROM old_rows", vec![])
            .await
            .is_err()
    );
    cleanup(schema).await
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn declared_write_properties_require_their_column() -> Result<()> {
    for property in [
        Req::Default("items".into(), "label".into()),
        Req::WriteConstraint("items".into(), "label".into()),
    ] {
        let mut s = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
        s.add_schema_object(table("reports", "events"))?;
        s.add_schema_object(table("archive", "old_rows"))?;
        let mut files = baseline(&s)?;
        let (db, schema) = fixture().await;
        db.migrate(graph(&files)?).await?;
        db.execute_sql("INSERT INTO old_rows VALUES(42)", vec![])
            .await?;
        add_column(&mut s, "label")?;
        update(&mut files, &s, "")?;
        drop_object(&mut s, SchemaObjectKind::Table, "old_rows")?;
        let ingest = trigger(
            &db,
            "reports",
            "ingest",
            "events",
            "INSERT INTO items(id) VALUES(NEW.id)",
        )
        .await?;
        s.add_declared_object(
            ingest,
            [
                Req::Column("events".into(), "id".into()),
                Req::Column("items".into(), "id".into()),
                property,
            ],
        )?;
        update(&mut files, &s, "old_rows")?;
        install(&db, &files, "reports").await?;
        db.execute_sql("INSERT INTO events VALUES(7)", vec![])
            .await?;
        assert_eq!(
            value(&db, "SELECT id FROM items WHERE label IS NULL").await?,
            7
        );
        assert_eq!(value(&db, "SELECT id FROM old_rows").await?, 42);
        db.migrate(graph(&files)?).await?;
        db.execute_sql("INSERT INTO events VALUES(8)", vec![])
            .await?;
        assert_eq!(
            value(&db, "SELECT count(*) FROM items WHERE label IS NULL").await?,
            2
        );
        assert!(
            db.query_sql("SELECT id FROM old_rows", vec![])
                .await
                .is_err()
        );
        cleanup(schema).await?;
    }
    Ok(())
}

#[tokio::test]
#[cfg_attr(feature = "postgres", ignore = "requires TEST_POSTGRES_URL")]
async fn compatible_view_append_leaves_archive_pending_for_existing_outputs() -> Result<()> {
    for (consumer, output, frozen) in [
        ("SELECT id FROM provider", "1", true),
        ("SELECT id FROM provider", "1", false),
        ("SELECT id FROM provider", "label", true),
        ("SELECT other AS id FROM provider", "1", true),
        ("SELECT other AS id FROM provider", "1", false),
        ("SELECT * FROM provider", "1", true),
    ] {
        let mut state = ProjectState::from_app_schema("stock", &SchemaSet::new().model::<Item>());
        state.add_schema_object(table("stock", "archive"))?;
        let mut provider = report("provider", "SELECT id FROM items");
        provider.app = "stock".into();
        state.add_schema_object(provider.clone())?;
        let base = baseline(&state)?;
        let mut files = base.clone();
        drop_object(&mut state, SchemaObjectKind::View, "provider")?;
        drop_object(&mut state, SchemaObjectKind::Table, "archive")?;
        if output == "label" {
            add_column(&mut state, "label")?;
        }
        provider.sql = format!("CREATE VIEW provider AS SELECT id, {output} AS other FROM items");
        state.add_schema_object(provider)?;
        if frozen {
            update(&mut files, &state, "archive")?;
        }
        state.add_schema_object(report("summary", consumer))?;
        update(&mut files, &state, if frozen { "" } else { "archive" })?;
        let (db, schema) = fixture().await;
        db.migrate(graph(&base)?).await?;
        db.execute_sql("INSERT INTO items VALUES(7)", vec![])
            .await?;
        db.execute_sql("INSERT INTO archive VALUES(42)", vec![])
            .await?;
        install(&db, &files, "reports").await?;
        let named_old = consumer == "SELECT id FROM provider";
        if named_old {
            assert_eq!(value(&db, "SELECT id FROM archive").await?, 42);
        } else if frozen {
            assert!(
                db.query_sql("SELECT id FROM archive", vec![])
                    .await
                    .is_err()
            );
        }
        let expected = if consumer.starts_with("SELECT other") {
            1
        } else {
            7
        };
        assert_eq!(value(&db, "SELECT id FROM summary").await?, expected);
        db.migrate(graph(&files)?).await?;
        assert!(
            db.query_sql("SELECT id FROM archive", vec![])
                .await
                .is_err()
        );
        assert_eq!(value(&db, "SELECT id FROM summary").await?, expected);
        if output == "label" {
            db.execute_sql("UPDATE items SET label=31", vec![]).await?;
            assert_eq!(value(&db, "SELECT other FROM provider").await?, 31);
        } else {
            assert_eq!(value(&db, "SELECT other FROM provider").await?, 1);
        }
        cleanup(schema).await?;
    }
    Ok(())
}
