#![cfg(feature = "sqlite")]

use che_orm::{
    Database, DatabaseValue, Migration, MigrationGraph, MigrationId, MigrationOperation, Model,
    SqlCompiler, SqliteDialect, TableState,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Debug, Model)]
#[orm(table = "canonical_dates", index("at"))]
struct Entry {
    #[orm(primary_key)]
    id: i64,
    at: Option<OffsetDateTime>,
    #[orm(auto_now_add)]
    created: OffsetDateTime,
    #[orm(auto_now)]
    updated: OffsetDateTime,
}

fn instant(text: &str) -> OffsetDateTime {
    OffsetDateTime::parse(text, &Rfc3339).unwrap()
}

#[tokio::test]
async fn writes_defaults_and_transactions_use_fixed_width_utc_without_changing_json() {
    let db = Database::connect_in_memory().await.unwrap();
    db.create_table::<Entry>().await.unwrap();
    for (text, expected) in [
        ("2026-01-01T00:00:00Z", "2026-01-01T00:00:00.000000000Z"),
        (
            "2026-01-01T03:00:00.1+03:00",
            "2026-01-01T00:00:00.100000000Z",
        ),
        (
            "2026-01-01T00:00:00.000000001Z",
            "2026-01-01T00:00:00.000000001Z",
        ),
        (
            "2025-12-31T23:00:00-01:00",
            "2026-01-01T00:00:00.000000000Z",
        ),
        ("0000-01-01T00:00:00Z", "0000-01-01T00:00:00.000000000Z"),
        (
            "9999-12-31T23:59:59.999999999Z",
            "9999-12-31T23:59:59.999999999Z",
        ),
    ] {
        let at = instant(text);
        let entry = db
            .create::<Entry>()
            .set(Entry::AT, Some(at))
            .execute()
            .await
            .unwrap();
        assert_eq!(entry.at.unwrap(), at);
        let raw = db
            .query_sql(
                "SELECT at, created, updated FROM canonical_dates WHERE id = ?",
                vec![DatabaseValue::Integer(entry.id)],
            )
            .await
            .unwrap();
        assert_eq!(raw[0].get::<String>(0).unwrap(), expected);
        for index in [1, 2] {
            let text = raw[0].get::<String>(index).unwrap();
            assert_eq!(text.len(), 30);
            assert!(text.ends_with("000000Z"));
        }
    }
    let at = instant("2026-02-01T12:00:00.123456789Z");
    db.transaction(move |tx| {
        Box::pin(async move {
            let entry = tx
                .create::<Entry>()
                .set(Entry::AT, Some(at))
                .execute()
                .await?;
            let updated = tx
                .update::<Entry>(entry.id)
                .set(Entry::AT, None::<OffsetDateTime>)
                .execute()
                .await?
                .unwrap();
            assert!(updated.at.is_none());
            assert!(updated.updated >= entry.updated);
            Ok(())
        })
    })
    .await
    .unwrap();
    let json =
        che_orm::serde_json::to_value(che_orm::Rfc3339Timestamp(&instant("2026-01-01T00:00:00Z")))
            .unwrap();
    assert_eq!(json, "2026-01-01T00:00:00Z");
}

#[tokio::test]
async fn datetime_sort_and_range_queries_use_ordinary_indexes() {
    let db = Database::connect_in_memory().await.unwrap();
    db.create_table::<Entry>().await.unwrap();
    let query = Entry::query()
        .filter(Entry::AT.gt(Some(instant("2026-01-01T00:00:00Z"))))
        .order_by(Entry::AT.desc())
        .limit(50)
        .into_ast()
        .unwrap();
    let compiled = SqlCompiler::<SqliteDialect>::compile(&query);
    assert!(!compiled.sql.contains("COLLATE"));
    let plan = db
        .query_sql(
            format!("EXPLAIN QUERY PLAN {}", compiled.sql),
            compiled.params,
        )
        .await
        .unwrap();
    let details = plan
        .iter()
        .map(|row| row.get::<String>(3).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(details.contains("canonical_dates_idx_0"), "{details}");
    assert!(!details.contains("TEMP B-TREE"), "{details}");
}

fn migrations() -> (Migration, Migration) {
    let initial = Migration::new(
        MigrationId {
            app: "dates".into(),
            name: "0001_initial".into(),
        },
        vec![],
        vec![MigrationOperation::CreateTable {
            table: TableState::from(&Entry::schema()),
        }],
    );
    let normalize = Migration::new(
        MigrationId {
            app: "dates".into(),
            name: "0002_canonical".into(),
        },
        vec![initial.id.clone()],
        vec![MigrationOperation::NormalizeSqliteDateTimes {
            table: "canonical_dates".into(),
            columns: vec!["at".into(), "created".into(), "updated".into()],
        }],
    );
    (initial, normalize)
}

#[tokio::test]
async fn invalid_legacy_values_roll_back_the_rebuild_and_history() {
    let db = Database::connect_in_memory().await.unwrap();
    let (initial, normalize) = migrations();
    db.migrate(MigrationGraph::new(vec![initial.clone()]).unwrap())
        .await
        .unwrap();
    for value in ["2026-01-01T03:00:00.123456789+03:00", "invalid"] {
        db.execute_sql(
            "INSERT INTO canonical_dates (at) VALUES (?)",
            vec![DatabaseValue::Text(value.into())],
        )
        .await
        .unwrap();
    }
    assert!(
        db.migrate(MigrationGraph::new(vec![initial.clone(), normalize.clone()]).unwrap())
            .await
            .is_err()
    );
    let rows = db
        .query_sql("SELECT at FROM canonical_dates ORDER BY id", vec![])
        .await
        .unwrap();
    assert_eq!(
        rows[0].get::<String>(0).unwrap(),
        "2026-01-01T03:00:00.123456789+03:00"
    );
    assert_eq!(rows[1].get::<String>(0).unwrap(), "invalid");
    let history = db
        .query_sql("SELECT count(*) FROM che_migration_history", vec![])
        .await
        .unwrap();
    assert_eq!(history[0].get::<i64>(0).unwrap(), 1);
    db.execute_sql(
        "DELETE FROM canonical_dates WHERE at = ?",
        vec![DatabaseValue::Text("invalid".into())],
    )
    .await
    .unwrap();
    let graph = MigrationGraph::new(vec![initial, normalize]).unwrap();
    db.migrate(graph.clone()).await.unwrap();
    db.migrate(graph).await.unwrap();
    let rows = db
        .query_sql("SELECT at FROM canonical_dates", vec![])
        .await
        .unwrap();
    assert_eq!(
        rows[0].get::<String>(0).unwrap(),
        "2026-01-01T00:00:00.123456789Z"
    );
    let entry = db.query::<Entry>().first().await.unwrap().unwrap();
    assert_eq!(entry.at.unwrap().nanosecond(), 123456789);
    let rows = db
        .query_sql("PRAGMA index_list(canonical_dates)", vec![])
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
}

#[test]
fn normalization_is_explicit_validated_and_serialized_in_migration_source() {
    let (initial, normalize) = migrations();
    let source = che_orm::render_migration_rust(&normalize);
    assert!(source.contains("NormalizeSqliteDateTimes"));
    let checksum = normalize.checksum.clone();
    let graph = MigrationGraph::new(vec![initial.clone(), normalize.clone()]).unwrap();
    assert_eq!(
        graph.replay(Default::default()).unwrap(),
        MigrationGraph::new(vec![initial.clone()])
            .unwrap()
            .replay(Default::default())
            .unwrap()
    );
    let mut changed = normalize;
    changed.operations = vec![MigrationOperation::NormalizeSqliteDateTimes {
        table: "canonical_dates".into(),
        columns: vec!["created".into()],
    }];
    assert_ne!(changed.calculated_checksum(), checksum);
    for columns in [
        vec![],
        vec!["id".into()],
        vec!["missing".into()],
        vec!["at".into(), "at".into()],
    ] {
        let invalid = Migration::new(
            changed.id.clone(),
            changed.dependencies.clone(),
            vec![MigrationOperation::NormalizeSqliteDateTimes {
                table: "canonical_dates".into(),
                columns,
            }],
        );
        assert!(
            MigrationGraph::new(vec![initial.clone(), invalid])
                .unwrap()
                .replay(Default::default())
                .is_err()
        );
    }
}

#[tokio::test]
async fn normalization_rejects_unique_collisions_without_discarding_rows() {
    let db = Database::connect_in_memory().await.unwrap();
    let (initial, normalize) = migrations();
    let mut table = TableState::from(&Entry::schema());
    table
        .columns
        .iter_mut()
        .find(|column| column.name == "at")
        .unwrap()
        .unique = true;
    let initial = Migration::new(
        initial.id,
        initial.dependencies,
        vec![MigrationOperation::CreateTable { table }],
    );
    db.migrate(MigrationGraph::new(vec![initial.clone()]).unwrap())
        .await
        .unwrap();
    for text in ["2026-01-01T00:00:00Z", "2026-01-01T03:00:00+03:00"] {
        db.execute_sql(
            "INSERT INTO canonical_dates (at) VALUES (?)",
            vec![DatabaseValue::Text(text.into())],
        )
        .await
        .unwrap();
    }
    assert!(
        db.migrate(MigrationGraph::new(vec![initial, normalize]).unwrap())
            .await
            .is_err()
    );
    assert_eq!(db.count::<Entry>().await.unwrap(), 2);
    assert_eq!(
        db.query_sql("SELECT count(*) FROM che_migration_history", vec![])
            .await
            .unwrap()[0]
            .get::<i64>(0)
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn typed_writes_reject_utc_years_outside_the_sortable_contract() {
    let db = Database::connect_in_memory().await.unwrap();
    db.create_table::<Entry>().await.unwrap();
    let at = time::Date::from_calendar_date(-1, time::Month::January, 1)
        .unwrap()
        .midnight()
        .assume_utc();
    assert!(
        db.create::<Entry>()
            .set(Entry::AT, Some(at))
            .execute()
            .await
            .is_err()
    );
    assert_eq!(db.count::<Entry>().await.unwrap(), 0);
}

#[tokio::test]
async fn normalization_never_silently_rounds_unsupported_legacy_precision() {
    for value in ["2026-01-01T00:00:00.1234567891Z", "2016-12-31T23:59:60Z"] {
        let db = Database::connect_in_memory().await.unwrap();
        let (initial, normalize) = migrations();
        db.migrate(MigrationGraph::new(vec![initial.clone()]).unwrap())
            .await
            .unwrap();
        db.execute_sql(
            "INSERT INTO canonical_dates (at) VALUES (?)",
            vec![DatabaseValue::Text(value.into())],
        )
        .await
        .unwrap();
        assert!(
            db.migrate(MigrationGraph::new(vec![initial, normalize]).unwrap())
                .await
                .is_err()
        );
        assert_eq!(
            db.query_sql("SELECT at FROM canonical_dates", vec![])
                .await
                .unwrap()[0]
                .get::<String>(0)
                .unwrap(),
            value
        );
    }
}
