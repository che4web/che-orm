#![cfg(feature = "sqlite")]

use che_orm::{Database, DatabaseQuery, Model};

#[derive(Debug, Model)]
#[orm(table = "bound_query_parents")]
struct Parent {
    #[orm(primary_key)]
    id: i64,
    name: String,
}

#[derive(Debug, Model)]
#[orm(table = "bound_query_children")]
struct Child {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = Parent)]
    parent_id: i64,
}

#[tokio::test]
async fn queries_and_relations_keep_the_creating_database() {
    let first = Database::connect_in_memory().await.unwrap();
    let second = Database::connect_in_memory().await.unwrap();
    for (db, name) in [(&first, "first"), (&second, "second")] {
        db.create_table::<Parent>().await.unwrap();
        db.create_table::<Child>().await.unwrap();
        let parent = db
            .create::<Parent>()
            .set(Parent::NAME, name)
            .execute()
            .await
            .unwrap();
        db.create::<Child>()
            .set(Child::PARENT_ID, parent.id)
            .execute()
            .await
            .unwrap();
    }

    let query = first.query::<Parent>();
    assert_eq!(
        second
            .query::<Parent>()
            .first()
            .await
            .unwrap()
            .unwrap()
            .name,
        "second"
    );
    assert_eq!(
        query
            .filter(Parent::NAME.eq("first"))
            .order_by(Parent::ID.asc())
            .offset(0)
            .limit(1)
            .all()
            .await
            .unwrap()[0]
            .name,
        "first"
    );
    assert_eq!(
        first
            .query::<Parent>()
            .filter(Parent::NAME.eq("first"))
            .count()
            .await
            .unwrap(),
        1
    );
    assert!(
        first
            .query::<Parent>()
            .filter(Parent::NAME.eq("second"))
            .first()
            .await
            .unwrap()
            .is_none()
    );

    let joined = first
        .query::<Child>()
        .select_related(Child::PARENT)
        .filter(Child::ID.gt(0))
        .order_by(Child::ID.asc())
        .offset(0)
        .limit(1)
        .all()
        .await
        .unwrap();
    assert_eq!(joined[0].related.name, "first");
    assert_eq!(
        first
            .query::<Child>()
            .select_related(Child::PARENT)
            .count()
            .await
            .unwrap(),
        1
    );
    let prefetched = first
        .query::<Parent>()
        .prefetch_related(Child::PARENT.reverse())
        .filter(Parent::NAME.eq("first"))
        .order_by(Parent::ID.asc())
        .offset(0)
        .limit(1)
        .all()
        .await
        .unwrap();
    assert_eq!(prefetched[0].relations.0.related.len(), 1);
    assert_eq!(
        first
            .query::<Parent>()
            .prefetch_related(Child::PARENT.reverse())
            .count()
            .await
            .unwrap(),
        1
    );

    // Standalone querysets can still choose a database at execution time.
    let standalone = DatabaseQuery::new(Parent::query());
    assert_eq!(
        standalone.first(&second).await.unwrap().unwrap().name,
        "second"
    );
    let standalone_join = DatabaseQuery::new(Child::query()).select_related(Child::PARENT);
    assert_eq!(
        standalone_join.all(&second).await.unwrap()[0].related.name,
        "second"
    );
    let select = first
        .query::<Parent>()
        .filter(Parent::NAME.eq("second"))
        .into_select_query();
    assert_eq!(second.fetch_all(select).await.unwrap()[0].name, "second");
}
