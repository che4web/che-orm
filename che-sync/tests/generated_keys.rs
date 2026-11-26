#![cfg(feature = "sqlite")]

use che_orm::{
    Database, DatabaseValue, MigrationGraph, MigrationLayout, Model, ProjectState, SchemaSet,
    SqliteDialect, SqliteGeneratedKey, SqliteKeySequence, Uuid, ValidatedWrite,
};
use che_sync::{AllowAll, Registry, SyncServer};
use std::sync::Arc;

#[derive(Debug, Model)]
#[orm(table = "knowledge_project", sync, sync_name = "knowledge.project")]
struct Project {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(unique)]
    key: String,
}

#[derive(Debug, Model)]
#[orm(table = "tasks_task", sync, sync_name = "tasks.task")]
struct Task {
    #[orm(primary_key)]
    id: Uuid,
    #[orm(foreign_key = Project, on_delete = "cascade")]
    project_id: Uuid,
    #[orm(unique, default = "''", server_owned)]
    key: String,
}

#[tokio::test]
async fn generated_task_keys_are_captured_in_the_sync_journal() {
    let sequence = SqliteKeySequence::new("natural_key_sequence", &Project::KEY);
    let key = SqliteGeneratedKey::new(&Task::KEY, &Task::PROJECT_ID, &sequence, "TASK");
    let tasks = SchemaSet::new()
        .server_model::<Task>()
        .sqlite_generated_key(key);
    let knowledge = SchemaSet::new()
        .server_model::<Project>()
        .sqlite_key_sequence(sequence);
    // Install the consumer first to exercise cross-application dependency planning.
    let state =
        ProjectState::from_app_schemas([("tasks", &tasks), ("knowledge", &knowledge)]).unwrap();
    let schema = knowledge.merge(tasks);
    let export = che_orm::rusqlite::Connection::open_in_memory().unwrap();
    export
        .execute_batch(&schema.try_to_sql::<SqliteDialect>().unwrap())
        .unwrap();
    assert_eq!(
        export
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type = 'trigger'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        3
    );

    let empty = MigrationGraph::new(vec![]).unwrap();
    let files = empty
        .generate(&state, MigrationLayout::PerApp, "initial")
        .unwrap();
    let target = files
        .iter()
        .rfind(|file| file.id.app == "tasks")
        .unwrap()
        .id
        .clone();
    let graph = MigrationGraph::new(files).unwrap();
    let database = Database::connect_in_memory().await.unwrap();
    database
        .migrate_to(graph.clone(), Some(target))
        .await
        .unwrap();
    assert_eq!(database.migrate(graph.clone()).await.unwrap(), 0);
    assert!(
        graph
            .generate(&state, MigrationLayout::PerApp, "again")
            .unwrap()
            .is_empty()
    );

    let server = SyncServer::open_model_logs(
        database,
        Registry::from_schema(&schema).unwrap(),
        "test",
        Arc::new(AllowAll),
    )
    .await
    .unwrap();
    let project = Uuid::now_v7();
    let other_project = Uuid::now_v7();
    for (id, key) in [(project, "SYNC"), (other_project, "OTHER")] {
        server
            .write(ValidatedWrite::Create(
                Project::insert()
                    .set(Project::ID, id)
                    .set(Project::KEY, key),
            ))
            .await
            .unwrap();
    }
    let task = Uuid::now_v7();
    for (id, scope, expected) in [
        (task, project, "SYNC-TASK-0001"),
        (Uuid::now_v7(), other_project, "OTHER-TASK-0001"),
        (Uuid::now_v7(), project, "SYNC-TASK-0002"),
    ] {
        server
            .write(ValidatedWrite::Create(
                Task::insert()
                    .set(Task::ID, id)
                    .set(Task::PROJECT_ID, scope),
            ))
            .await
            .unwrap();
        assert_eq!(
            server
                .database()
                .get::<Task>(id)
                .await
                .unwrap()
                .unwrap()
                .key,
            expected
        );
    }
    let page = server.changes::<Task>(0, 10).await.unwrap();
    let keys: Vec<_> = page
        .operations
        .iter()
        .map(|operation| {
            operation
                .diff
                .iter()
                .find(|field| field.field == "key")
                .unwrap()
                .new_value
                .clone()
                .unwrap()
        })
        .collect();
    assert_eq!(
        keys,
        ["SYNC-TASK-0001", "OTHER-TASK-0001", "SYNC-TASK-0002"].map(serde_json::Value::from)
    );
    let db = server.database();
    assert!(
        db.execute_sql(
            "INSERT INTO tasks_task (id, project_id, key) VALUES (?1, ?2, 'EXPLICIT')",
            vec![
                DatabaseValue::Uuid(Uuid::now_v7()),
                DatabaseValue::Uuid(project)
            ]
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("server-generated")
    );
    assert!(
        db.execute_sql(
            "UPDATE tasks_task SET key = 'EDITED' WHERE id = ?1",
            vec![DatabaseValue::Uuid(task)]
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("cannot be changed")
    );

    // A failing multi-row insert must also roll back the first row's allocation.
    assert!(
        db.execute_sql(
            "INSERT INTO tasks_task (id, project_id) VALUES (?1, ?2), (?3, ?2)",
            vec![
                DatabaseValue::Uuid(Uuid::now_v7()),
                DatabaseValue::Uuid(project),
                DatabaseValue::Uuid(task)
            ]
        )
        .await
        .is_err()
    );
    let id = Uuid::now_v7();
    server
        .write(ValidatedWrite::Create(
            Task::insert()
                .set(Task::ID, id)
                .set(Task::PROJECT_ID, project),
        ))
        .await
        .unwrap();
    assert_eq!(
        db.get::<Task>(id).await.unwrap().unwrap().key,
        "SYNC-TASK-0003"
    );
}

#[test]
fn generated_keys_require_the_server_and_a_registered_sequence() {
    let sequence = SqliteKeySequence::new("natural_key_sequence", &Project::KEY);
    let key = || SqliteGeneratedKey::new(&Task::KEY, &Task::PROJECT_ID, &sequence, "TASK");
    let schema = SchemaSet::new()
        .client_model::<Project>()
        .client_model::<Task>()
        .sqlite_generated_key(key())
        .sqlite_key_sequence(SqliteKeySequence::new(
            "natural_key_sequence",
            &Project::KEY,
        ));
    assert!(
        ProjectState::from_app_schemas([("tasks", &schema)])
            .unwrap_err()
            .to_string()
            .contains("server schema")
    );

    let schema = SchemaSet::new()
        .server_model::<Project>()
        .server_model::<Task>()
        .sqlite_generated_key(key());
    assert!(
        ProjectState::from_app_schemas([("tasks", &schema)])
            .unwrap_err()
            .to_string()
            .contains("requires matching sequence registration")
    );
}
