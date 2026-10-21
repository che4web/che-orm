//! Submitted SELECTs must retain scope, tombstone, and authorization checks at every depth.
use super::*;
use che_orm::{ColumnRef, DatabaseValue, Expr, QueryAst, SelectQuery};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Model)]
#[orm(table = "subquery_candidates")]
struct Candidate {
    #[orm(primary_key)]
    id: i64,
    note_id: Uuid,
    body: String,
}
struct ReadGate(AtomicBool);
impl che_sync::AccessPolicy for ReadGate {
    fn authorize_scope(&self, _: &str, _: &str) -> Result<(), OrmError> {
        if self.0.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(OrmError::Configuration("scope access revoked".into()))
        }
    }
    fn validate(&self, _: &str, _: &str, _: &Entity, _: &Entity) -> Result<(), OrmError> {
        Ok(())
    }
}
struct Fixture {
    raw: Database,
    server: SyncServer,
    gate: Arc<ReadGate>,
    visible: Uuid,
    foreign: Uuid,
    cleanup: Option<(Database, String)>,
}
impl Fixture {
    async fn open() -> Self {
        let (db, cleanup) = database().await;
        let registry = Registry::new()
            .register::<Note>()
            .unwrap()
            .register::<Child>()
            .unwrap();
        migrate(&db, &registry, SchemaRole::Server).await.unwrap();
        db.create_table::<Candidate>().await.unwrap();
        let mut ids = Vec::new();
        for (title, scope, deleted) in [
            ("visible", "scope", false),
            ("foreign secret", "other_scope", false),
            ("deleted secret", "scope", true),
        ] {
            let note = db
                .create::<Note>()
                .set(Note::TITLE, title)
                .set(Note::ENABLED, true)
                .set(Note::SYNC_SCOPE, scope)
                .set(
                    Note::DELETED_AT,
                    deleted.then(che_orm::time::OffsetDateTime::now_utc),
                )
                .execute()
                .await
                .unwrap();
            db.create::<Candidate>()
                .set(Candidate::NOTE_ID, note.id)
                .set(Candidate::BODY, "untouched")
                .execute()
                .await
                .unwrap();
            ids.push(note.id);
        }
        let gate = Arc::new(ReadGate(AtomicBool::new(true)));
        let server = SyncServer::open(db.clone(), registry, "scope", "trusted-user", gate.clone())
            .await
            .unwrap();
        Self {
            raw: db,
            server,
            gate,
            visible: ids[0],
            foreign: ids[1],
            cleanup,
        }
    }
    async fn close(self) {
        if let Some((admin, schema)) = self.cleanup {
            admin
                .execute_sql(format!("DROP SCHEMA \"{schema}\" CASCADE"), vec![])
                .await
                .unwrap();
        }
    }
}
fn membership<M: Model>(column: ColumnRef, query: SelectQuery<M>, projection: ColumnRef) -> Expr {
    let QueryAst::Select(mut inner) = query.into_ast().unwrap() else {
        unreachable!()
    };
    inner.columns = vec![projection];
    Expr::InQuery {
        left: Box::new(Expr::Column(column)),
        query: Box::new(inner),
    }
}
fn notes(title: &str) -> Expr {
    membership(
        Candidate::NOTE_ID.column(),
        Note::query().filter(Note::TITLE.eq(title)),
        Note::ID.column(),
    )
}
fn candidates(title: &str) -> SelectQuery<Candidate> {
    Candidate::query().filter(Candidate::BODY.eq("untouched").and(notes(title)))
}

#[tokio::test]
#[cfg_attr(
    feature = "postgres",
    ignore = "requires TEST_POSTGRES_URL and a live PostgreSQL server"
)]
async fn subquery_reads_and_counts_keep_scope_and_tombstone_filters() {
    let fixture = Fixture::open().await;
    let db = fixture.server.database();
    assert!(db.get::<Note>(fixture.foreign).await.unwrap().is_none());
    for title in ["foreign secret", "deleted secret"] {
        assert!(db.fetch_all(candidates(title)).await.unwrap().is_empty());
        assert_eq!(db.count_query(candidates(title)).await.unwrap(), 0);
        db.transaction(move |tx| {
            Box::pin(async move {
                assert!(tx.fetch_all(candidates(title)).await?.is_empty());
                Ok(())
            })
        })
        .await
        .unwrap();
    }
    let visible = db.fetch_all(candidates("visible")).await.unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].note_id, fixture.visible);
    fixture.close().await;
}

#[tokio::test]
#[cfg_attr(
    feature = "postgres",
    ignore = "requires TEST_POSTGRES_URL and a live PostgreSQL server"
)]
async fn subqueries_nested_in_other_selects_and_join_predicates_are_scoped() {
    let fixture = Fixture::open().await;
    let nested = membership(
        Candidate::NOTE_ID.column(),
        candidates("foreign secret"),
        Candidate::NOTE_ID.column(),
    );
    assert!(
        fixture
            .server
            .database()
            .fetch_all(Candidate::query().filter(nested))
            .await
            .unwrap()
            .is_empty()
    );
    let QueryAst::Select(mut query) = Candidate::query().into_ast().unwrap() else {
        unreachable!()
    };
    let Expr::InQuery { query: inner, .. } = notes("foreign secret") else {
        unreachable!()
    };
    query.joins.push(che_orm::JoinAst {
        table: che_orm::TableRef::new(Note::table_name()),
        alias: "joined_notes",
        kind: che_orm::JoinType::Inner,
        on: Expr::Compare {
            left: Box::new(Expr::Column(Candidate::NOTE_ID.column())),
            op: che_orm::CompareOp::Eq,
            right: Box::new(Expr::Column(ColumnRef::new("joined_notes", "id"))),
        }
        .and(Expr::InQuery {
            left: Box::new(Expr::Value(DatabaseValue::Uuid(fixture.foreign))),
            query: inner,
        }),
    });
    let rows = fixture
        .server
        .database()
        .fetch_all(Candidate::query().filter(Expr::InQuery {
            left: Box::new(Expr::Column(Candidate::ID.column())),
            query: Box::new(che_orm::SelectAst {
                columns: vec![Candidate::ID.column()],
                ..query
            }),
        }))
        .await
        .unwrap();
    assert!(rows.is_empty());
    fixture.close().await;
}

#[tokio::test]
#[cfg_attr(
    feature = "postgres",
    ignore = "requires TEST_POSTGRES_URL and a live PostgreSQL server"
)]
async fn mutation_predicate_subqueries_cannot_observe_foreign_or_deleted_notes() {
    let fixture = Fixture::open().await;
    let db = fixture.server.database();
    let changed = db
        .execute(
            Candidate::update()
                .set(Candidate::BODY, "leaked")
                .filter(notes("foreign secret"))
                .into_ast()
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(changed.rows_affected, 0);
    let saved = che_orm::ValidatedWrite::Update(
        Candidate::update()
            .set(Candidate::BODY, "leaked")
            .filter(notes("deleted secret")),
    )
    .save(db)
    .await
    .unwrap();
    assert!(saved.is_none());
    db.transaction(move |tx| {
        Box::pin(async move {
            let changed = che_orm::ValidatedWrite::Update(
                Candidate::update()
                    .set(Candidate::BODY, "leaked")
                    .filter(notes("foreign secret")),
            )
            .save_in(tx)
            .await?;
            assert!(changed.is_none());
            let deleted = tx
                .execute(
                    Candidate::delete()
                        .filter(notes("deleted secret"))
                        .into_ast()?,
                )
                .await?;
            assert_eq!(deleted.rows_affected, 0);
            Ok(())
        })
    })
    .await
    .unwrap();
    assert!(
        fixture
            .raw
            .all::<Candidate>()
            .await
            .unwrap()
            .iter()
            .all(|row| row.body == "untouched")
    );
    fixture.close().await;
}

#[tokio::test]
#[cfg_attr(
    feature = "postgres",
    ignore = "requires TEST_POSTGRES_URL and a live PostgreSQL server"
)]
async fn inner_select_authorization_is_rechecked_in_all_ast_entry_points() {
    let fixture = Fixture::open().await;
    let db = fixture.server.database();
    fixture.gate.0.store(false, Ordering::SeqCst);
    // Non-sync data remains available; embedding a sync SELECT requires access.
    assert_eq!(db.all::<Candidate>().await.unwrap().len(), 3);
    assert!(db.fetch_all(candidates("visible")).await.is_err());
    assert!(db.count_query(candidates("visible")).await.is_err());
    assert!(
        db.execute(candidates("visible").into_ast().unwrap())
            .await
            .is_err()
    );
    assert!(
        db.execute(
            Candidate::update()
                .set(Candidate::BODY, "forbidden")
                .filter(notes("visible"))
                .into_ast()
                .unwrap()
        )
        .await
        .is_err()
    );
    assert!(
        che_orm::ValidatedWrite::Update(
            Candidate::update()
                .set(Candidate::BODY, "forbidden")
                .filter(notes("visible"))
        )
        .save(db)
        .await
        .is_err()
    );
    db.transaction(move |tx| {
        Box::pin(async move {
            assert!(tx.fetch_all(candidates("visible")).await.is_err());
            assert!(tx.execute(candidates("visible").into_ast()?).await.is_err());
            assert!(
                tx.execute(Candidate::delete().filter(notes("visible")).into_ast()?)
                    .await
                    .is_err()
            );
            assert!(
                che_orm::ValidatedWrite::Update(
                    Candidate::update()
                        .set(Candidate::BODY, "forbidden")
                        .filter(notes("visible"))
                )
                .save_in(tx)
                .await
                .is_err()
            );
            Ok(())
        })
    })
    .await
    .unwrap();
    assert!(
        fixture
            .raw
            .all::<Candidate>()
            .await
            .unwrap()
            .iter()
            .all(|row| row.body == "untouched")
    );
    fixture.close().await;
}

#[tokio::test]
#[cfg_attr(
    feature = "postgres",
    ignore = "requires TEST_POSTGRES_URL and a live PostgreSQL server"
)]
async fn client_runtime_hides_tombstones_inside_subqueries_and_captured_writes() {
    let (db, cleanup) = database().await;
    let registry = Registry::new()
        .register::<Note>()
        .unwrap()
        .register::<Child>()
        .unwrap();
    migrate(&db, &registry, SchemaRole::Client).await.unwrap();
    db.create_table::<Candidate>().await.unwrap();
    let note = db
        .create::<Note>()
        .set(Note::TITLE, "deleted secret")
        .set(Note::ENABLED, true)
        .set(
            Note::DELETED_AT,
            Some(che_orm::time::OffsetDateTime::now_utc()),
        )
        .execute()
        .await
        .unwrap();
    db.create::<Candidate>()
        .set(Candidate::NOTE_ID, note.id)
        .set(Candidate::BODY, "untouched")
        .execute()
        .await
        .unwrap();
    let visible = db
        .create::<Note>()
        .set(Note::TITLE, "visible")
        .set(Note::ENABLED, true)
        .execute()
        .await
        .unwrap();
    let client = SyncClient::open(db, registry, "scope").await.unwrap();
    assert!(
        client
            .database()
            .get::<Note>(note.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        client
            .database()
            .fetch_all(candidates("deleted secret"))
            .await
            .unwrap()
            .is_empty()
    );
    let hidden_subquery = || {
        let Expr::InQuery { query, .. } = notes("deleted secret") else {
            unreachable!()
        };
        Expr::InQuery {
            left: Box::new(Expr::Value(DatabaseValue::Uuid(note.id))),
            query,
        }
    };
    let update = || {
        Note::update()
            .set(Note::TITLE, "leaked")
            .filter(Note::ID.eq(visible.id))
            .filter(hidden_subquery())
    };
    assert_eq!(
        client
            .database()
            .execute(update().into_ast().unwrap())
            .await
            .unwrap()
            .rows_affected,
        0
    );
    assert!(
        che_orm::ValidatedWrite::Update(update())
            .save(client.database())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        client
            .database()
            .get::<Note>(visible.id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "visible"
    );
    if let Some((admin, schema)) = cleanup {
        admin
            .execute_sql(format!("DROP SCHEMA \"{schema}\" CASCADE"), vec![])
            .await
            .unwrap();
    }
}
