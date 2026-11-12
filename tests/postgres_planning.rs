use che_orm::{MigrationOperation, Model, PostgresSchemaEditor, ProjectState, TableState};

#[derive(Debug, Model)]
#[orm(table = "planning_parent")]
struct Parent {
    #[orm(primary_key)]
    id: i64,
}
#[derive(Debug, Model)]
#[orm(table = "planning_child")]
struct Child {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key=Parent)]
    parent_id: i64,
}

#[test]
fn whole_migration_preview_resolves_and_tracks_deferred_foreign_keys() {
    let operations = vec![
        MigrationOperation::CreateTable {
            table: TableState::from(&Child::schema()),
        },
        MigrationOperation::RenameColumn {
            table: "planning_child".into(),
            old_name: "parent_id".into(),
            new_name: "renamed_parent".into(),
        },
        MigrationOperation::CreateTable {
            table: TableState::from(&Parent::schema()),
        },
    ];
    let plan = PostgresSchemaEditor::plan_migration(&operations, &ProjectState::default()).unwrap();
    assert!(plan.deferred_foreign_keys.is_empty());
    let foreign = plan
        .statements
        .iter()
        .filter(|sql| sql.contains("FOREIGN KEY"))
        .collect::<Vec<_>>();
    assert_eq!(foreign.len(), 1);
    assert!(foreign[0].contains("\"renamed_parent\""));
    assert!(plan.statements.last().unwrap().contains("ADD CONSTRAINT"));
    assert!(
        PostgresSchemaEditor::plan_migration(&operations[..1], &ProjectState::default()).is_err()
    );
}
