//! Isolated SQLite comparison benchmark, not an HTTP throughput benchmark.
#![cfg(feature = "sqlite")]

use che_orm::rusqlite::{Connection, params};
use std::{hint::black_box, time::Instant};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};

fn ids(db: &Connection, order: &str) -> Vec<i64> {
    db.prepare(&format!(
        "SELECT id FROM dates ORDER BY {order} DESC LIMIT 50"
    ))
    .unwrap()
    .query_map([], |row| row.get(0))
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

#[test]
#[ignore = "manual release-mode performance measurement"]
fn sqlite_legacy_collation_vs_canonical_text() {
    let mut db = Connection::open_in_memory().unwrap();
    db.create_collation("legacy_datetime", |left, right| {
        OffsetDateTime::parse(left, &Rfc3339)
            .unwrap()
            .cmp(&OffsetDateTime::parse(right, &Rfc3339).unwrap())
    })
    .unwrap();
    db.execute_batch("CREATE TABLE dates (id INTEGER PRIMARY KEY, legacy TEXT NOT NULL, canonical TEXT NOT NULL)").unwrap();
    let transaction = db.transaction().unwrap();
    {
        let mut insert = transaction
            .prepare("INSERT INTO dates VALUES (?, ?, ?)")
            .unwrap();
        for id in 1..=10_000 {
            let at = OffsetDateTime::UNIX_EPOCH + Duration::seconds(id);
            let legacy = at.format(&Rfc3339).unwrap();
            let canonical = format!("{}.000000000Z", legacy.trim_end_matches('Z'));
            insert.execute(params![id, legacy, canonical]).unwrap();
        }
    }
    transaction.commit().unwrap();
    let expected = ids(&db, "legacy COLLATE legacy_datetime");
    assert_eq!(expected, ids(&db, "canonical"));
    for (name, order) in [
        ("legacy RFC3339 collation", "legacy COLLATE legacy_datetime"),
        ("canonical UTC TEXT", "canonical"),
    ] {
        for _ in 0..10 {
            black_box(ids(&db, order));
        }
        let started = Instant::now();
        for _ in 0..100 {
            black_box(ids(&db, order));
        }
        println!(
            "{name}: {:.3} ms/query (10,000 rows, LIMIT 50, no index)",
            started.elapsed().as_secs_f64() * 10.0
        );
    }
    db.execute_batch("CREATE INDEX dates_canonical_idx ON dates(canonical)")
        .unwrap();
    assert_eq!(expected, ids(&db, "canonical"));
    let started = Instant::now();
    for _ in 0..100 {
        black_box(ids(&db, "canonical"));
    }
    println!(
        "canonical UTC TEXT + index: {:.3} ms/query",
        started.elapsed().as_secs_f64() * 10.0
    );
}
