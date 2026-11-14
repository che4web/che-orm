//! Migration history validation is public and independent of the selected backend.
use che_orm::{Migration, MigrationGraph, MigrationId, migration::pending_operation_migrations};
use std::collections::HashMap;

fn migration(name: &str, dependencies: Vec<MigrationId>) -> Migration {
    Migration::new(
        MigrationId {
            app: "history".into(),
            name: name.into(),
        },
        dependencies,
        vec![],
    )
}

fn key(migration: &Migration) -> (String, String) {
    (migration.id.app.clone(), migration.id.name.clone())
}

#[test]
fn public_history_validator_returns_only_pending_migrations_in_dependency_order() {
    let first = migration("0001", vec![]);
    let second = migration("0002", vec![first.id.clone()]);
    let third = migration("0003", vec![second.id.clone()]);
    let graph = MigrationGraph::new(vec![third.clone(), second.clone(), first.clone()]).unwrap();
    let applied = HashMap::from([(key(&first), first.checksum.clone())]);

    let pending = pending_operation_migrations(&graph, &applied).unwrap();
    assert_eq!(pending, vec![&second, &third]);
}

#[test]
fn public_history_validator_rejects_divergent_missing_and_inconsistent_history() {
    let first = migration("0001", vec![]);
    let second = migration("0002", vec![first.id.clone()]);
    let graph = MigrationGraph::new(vec![second.clone(), first.clone()]).unwrap();

    let divergent = HashMap::from([(key(&first), "different checksum".into())]);
    assert!(
        pending_operation_migrations(&graph, &divergent)
            .unwrap_err()
            .to_string()
            .contains("divergent operation migration")
    );

    let unknown = migration("unregistered", vec![]);
    let missing = HashMap::from([(key(&unknown), unknown.checksum)]);
    assert!(
        pending_operation_migrations(&graph, &missing)
            .unwrap_err()
            .to_string()
            .contains("missing operation migration")
    );

    let inconsistent = HashMap::from([(key(&second), second.checksum)]);
    assert!(
        pending_operation_migrations(&graph, &inconsistent)
            .unwrap_err()
            .to_string()
            .contains("missing dependency")
    );
}
