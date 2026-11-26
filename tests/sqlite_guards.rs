#![cfg(feature = "sqlite")]

use che_orm::{
    MigrationGraph, MigrationLayout, Model, ProjectState, SchemaSet, SqliteDialect, SqliteGuard,
    TriggerCondition, TriggerField,
    migration::sqlite_executor::apply_operation_migrations_to_on_connection, rusqlite::Connection,
};

#[derive(Debug, Model)]
#[orm(table = "knowledge_module")]
struct Module {
    #[orm(primary_key)]
    id: i64,
    project_id: i64,
}

#[derive(Debug, Model)]
#[orm(table = "tasks_task")]
struct Task {
    #[orm(primary_key)]
    id: i64,
    project_id: i64,
    module_id: Option<i64>,
}

fn task_scope() -> SqliteGuard<Task> {
    let module = TriggerField::new_row(&Task::MODULE_ID);
    let project = TriggerField::new_row(&Task::PROJECT_ID);
    let matches_module = TriggerField::source(&Module::ID)
        .eq(module)
        .and(TriggerField::source(&Module::PROJECT_ID).eq(project));
    SqliteGuard::reject_when(
        "tasks_task_module_project",
        module
            .is_not_null()
            .and(TriggerCondition::not_exists::<Module>(matches_module)),
        "task's module must belong to its project",
    )
    .before_insert()
    .before_update_of(&Task::MODULE_ID)
    .before_update_of(&Task::PROJECT_ID)
}

#[test]
fn task_scope_guard_is_compiled_into_migrations_and_enforced() {
    let tasks = SchemaSet::new().model::<Task>().sqlite_guard(task_scope());
    let knowledge = SchemaSet::new().model::<Module>();
    let desired =
        ProjectState::from_app_schemas([("tasks", &tasks), ("knowledge", &knowledge)]).unwrap();
    assert_eq!(desired.objects().len(), 2);
    let sql = tasks
        .clone()
        .merge(knowledge)
        .try_to_sql::<SqliteDialect>()
        .unwrap();
    assert!(sql.contains("CREATE TRIGGER \"tasks_task_module_project_update\""));

    let empty = MigrationGraph::new(vec![]).unwrap();
    let migrations = empty
        .generate(&desired, MigrationLayout::PerApp, "initial")
        .unwrap();
    let target = migrations
        .iter()
        .rfind(|file| file.id.app == "tasks")
        .unwrap()
        .id
        .clone();
    let graph = MigrationGraph::new(migrations).unwrap();
    let mut db = Connection::open_in_memory().unwrap();
    apply_operation_migrations_to_on_connection(&mut db, graph.clone(), Some(target)).unwrap();

    db.execute_batch(
        "INSERT INTO knowledge_module VALUES (10, 1), (20, 2);
         INSERT INTO tasks_task VALUES (1, 1, 10), (2, 1, NULL);",
    )
    .unwrap();
    for sql in [
        "INSERT INTO tasks_task VALUES (3, 1, 20)",
        "INSERT INTO tasks_task VALUES (3, 1, 999)",
        "UPDATE tasks_task SET project_id = 2 WHERE id = 1",
        "UPDATE tasks_task SET module_id = 20 WHERE id = 2",
    ] {
        let error = db.execute(sql, []).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("task's module must belong to its project"),
            "{error}"
        );
    }
    assert_eq!(
        db.query_row("SELECT project_id FROM tasks_task WHERE id = 1", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
    assert_eq!(
        apply_operation_migrations_to_on_connection(&mut db, graph.clone(), None).unwrap(),
        0
    );
    assert!(
        graph
            .generate(&desired, MigrationLayout::PerApp, "again")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn guards_validate_referenced_models_and_event_scope_before_generation() {
    let tasks = SchemaSet::new().model::<Task>().sqlite_guard(task_scope());
    let error = ProjectState::from_app_schemas([("tasks", &tasks)]).unwrap_err();
    assert!(error.to_string().contains("unknown table knowledge_module"));

    let guard = SqliteGuard::<Task>::reject_when(
        "invalid_guard",
        TriggerField::old_row(&Task::PROJECT_ID).is_not_null(),
        "OLD is unavailable on insert",
    )
    .before_insert();
    let tasks = SchemaSet::new().model::<Task>().sqlite_guard(guard);
    let error = ProjectState::from_app_schemas([("tasks", &tasks)]).unwrap_err();
    assert!(error.to_string().contains("outside its row scope"));
}
