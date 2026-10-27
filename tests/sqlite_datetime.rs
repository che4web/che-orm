#![cfg(feature = "sqlite")]

use che_orm::{Database, DatabaseValue, Model, PostgresDialect, QueryAst, SqlCompiler};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Debug, Model)]
#[orm(table = "datetime_regressions")]
struct Timestamp {
    #[orm(primary_key)]
    id: i64,
    #[orm(db_column = "occurred_at")]
    at: Option<OffsetDateTime>,
    #[orm(auto_now_add)]
    created: OffsetDateTime,
}

#[derive(Debug, Model)]
#[orm(table = "datetime_links")]
struct TimestampLink {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = Timestamp)]
    timestamp_id: i64,
}

fn instant(value: &str) -> OffsetDateTime {
    OffsetDateTime::parse(value, &Rfc3339).unwrap()
}

#[tokio::test]
async fn legacy_timestamps_compare_and_sort_without_rewriting_rows() {
    let db = Database::connect_in_memory().await.unwrap();
    db.create_table::<Timestamp>().await.unwrap();
    let values = [
        Some("2026-01-01T00:00:00Z"),
        Some("2026-01-01T00:00:00.1Z"),
        Some("2026-01-01T00:00:00.000Z"),
        Some("2026-01-01T03:00:00+03:00"),
        Some("2026-01-01T00:00:00.000000001Z"),
        Some("2026-01-01T00:00:00.100000001Z"),
        None,
    ];
    for value in values {
        db.execute_sql(
            "INSERT INTO datetime_regressions (occurred_at) VALUES (?)",
            vec![value.map_or(DatabaseValue::Null, |v| DatabaseValue::Text(v.into()))],
        )
        .await
        .unwrap();
    }
    let exact = instant("2026-01-01T00:00:00Z");
    let rows = db
        .fetch_all(
            Timestamp::query()
                .filter(Timestamp::AT.gt(Some(exact)))
                .order_by(Timestamp::AT.asc()),
        )
        .await
        .unwrap();
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [5, 2, 6]);
    let rows = db
        .fetch_all(
            Timestamp::query()
                .filter(Timestamp::AT.eq(Some(exact)))
                .order_by(Timestamp::ID.asc()),
        )
        .await
        .unwrap();
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [1, 3, 4]);
    let rows = db
        .fetch_all(
            Timestamp::query()
                .filter(Timestamp::AT.is_not_null())
                .order_by(Timestamp::AT.desc())
                .order_by(Timestamp::ID.asc()),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        [6, 2, 5, 1, 3, 4]
    );
    assert_eq!(
        db.fetch_all(Timestamp::query().filter(Timestamp::AT.in_values([Some(exact)])))
            .await
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        db.fetch_all(Timestamp::query().filter(Timestamp::AT.is_null()))
            .await
            .unwrap()
            .len(),
        1
    );

    // SQLite's historical millisecond default must compare equal to its decoded value.
    let created = rows[0].created;
    assert!(
        !db.fetch_all(Timestamp::query().filter(Timestamp::CREATED.eq(created)))
            .await
            .unwrap()
            .is_empty()
    );
    let raw = db
        .query_sql(
            "SELECT occurred_at FROM datetime_regressions WHERE id = 3",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(raw[0].get::<String>(0).unwrap(), "2026-01-01T00:00:00.000Z");

    db.create_table::<TimestampLink>().await.unwrap();
    for id in 1..=6 {
        db.create::<TimestampLink>()
            .set(TimestampLink::TIMESTAMP_ID, id)
            .execute()
            .await
            .unwrap();
    }
    let related_at = TimestampLink::TIMESTAMP.related_field(Timestamp::AT);
    let joined = db
        .query::<TimestampLink>()
        .select_related(TimestampLink::TIMESTAMP)
        .filter(related_at.gt(Some(exact)))
        .order_by(related_at.asc())
        .all(&db)
        .await
        .unwrap();
    assert_eq!(
        joined.iter().map(|row| row.related.id).collect::<Vec<_>>(),
        [5, 2, 6]
    );

    db.transaction(move |tx| {
        Box::pin(async move {
            let rows = tx
                .fetch_all(Timestamp::query().filter(Timestamp::AT.eq(Some(exact))))
                .await?;
            assert_eq!(rows.len(), 3);
            let result = tx
                .execute(
                    Timestamp::update()
                        .filter(Timestamp::AT.eq(Some(exact)))
                        .set(Timestamp::AT, Some(instant("2026-01-02T00:00:00Z")))
                        .into_ast()?,
                )
                .await?;
            assert_eq!(result.rows_affected, 3);
            Ok(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn offset_without_limit_executes_and_counts_on_sqlite() {
    let db = Database::connect_in_memory().await.unwrap();
    db.create_table::<Timestamp>().await.unwrap();
    for _ in 0..3 {
        db.create::<Timestamp>()
            .set(Timestamp::AT, None::<OffsetDateTime>)
            .execute()
            .await
            .unwrap();
    }
    let rows = db
        .fetch_all(Timestamp::query().order_by(Timestamp::ID.asc()).offset(1))
        .await
        .unwrap();
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [2, 3]);
    assert_eq!(
        db.count_query(Timestamp::query().offset(1)).await.unwrap(),
        2
    );
    assert!(
        db.fetch_all(Timestamp::query().offset(10))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.fetch_all(Timestamp::query().offset(1).limit(1))
            .await
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn postgres_keeps_native_timestamp_comparison_and_offset() {
    let query = Timestamp::query()
        .filter(Timestamp::AT.gt(Some(instant("2026-01-01T00:00:00Z"))))
        .order_by(Timestamp::AT.asc())
        .offset(1)
        .into_ast()
        .unwrap();
    let compiled = SqlCompiler::<PostgresDialect>::compile(&query);
    assert!(!compiled.sql.contains("COLLATE"));
    assert!(!compiled.sql.contains("LIMIT"));
    assert!(compiled.sql.ends_with(" OFFSET 1"));
    assert!(matches!(query, QueryAst::Select(_)));
}
