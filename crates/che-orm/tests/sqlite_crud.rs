use std::time::{Duration, SystemTime, UNIX_EPOCH};

use che_orm::{
    AnnotationField, Choice, Model, ModelEvent, NaiveDateTime, Q, SqliteBackend, SqliteValue,
    create_table_sql,
};
use serde_json::{Value, json};
use tokio::{sync::broadcast, time::timeout};

#[derive(Debug, Clone, Model)]
#[model(table = "users")]
struct User {
    #[field(primary_key)]
    id: i64,

    #[field(unique, max_length = 255)]
    email: String,

    name: String,

    #[field(default = false)]
    is_active: bool,
}

#[derive(Debug, Clone, Model)]
#[model(table = "tasks")]
struct Task {
    #[field(primary_key)]
    id: i64,

    title: String,

    #[field(auto_now_add)]
    created_at: NaiveDateTime,

    #[field(auto_now)]
    updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Choice)]
enum TaskStatus {
    New,
    InProgress,
    Done,
}

#[derive(Debug, Clone, Model)]
#[model(table = "choice_tasks")]
struct ChoiceTask {
    #[field(primary_key)]
    id: i64,
    status: TaskStatus,
}

#[derive(Debug, Clone, Model)]
#[model(table = "json_tasks")]
struct JsonTask {
    #[field(primary_key)]
    id: i64,

    title: String,

    metadata: Value,

    optional_metadata: Option<Value>,
}

#[derive(Debug, Clone, Model)]
#[model(table = "metrics")]
struct Metric {
    #[field(primary_key)]
    id: i64,
    score: Option<i64>,
    value: f64,
}

#[derive(Debug, Clone, Model)]
#[model(table = "scalar_types")]
struct ScalarTypes {
    #[field(primary_key)]
    id: i64,
    small: i32,
    unsigned: u32,
    ratio: f32,
}

#[tokio::test]
async fn broadcast_signals_receive_create_and_update_snapshots() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();
    db.create_table::<Task>().await.unwrap();
    let mut events = db.signals().subscribe::<User>();
    let mut audit_events = db.signals().subscribe::<User>();
    let mut task_events = db.signals().subscribe::<Task>();

    let user = User::objects(&db)
        .create()
        .set("email", "signals@example.com")
        .set("name", "Before")
        .execute()
        .await
        .unwrap();
    Task::objects(&db)
        .create()
        .set("title", "Task event")
        .execute()
        .await
        .unwrap();
    User::objects(&db)
        .update_fields(user.id)
        .set("name", "After")
        .execute()
        .await
        .unwrap();

    let first = timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    let second = timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    let third = timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        first,
        ModelEvent::PostSave(event) if event.created && event.object["name"] == "Before"
    ));
    assert!(matches!(
        second,
        ModelEvent::PostSave(event) if !event.created && event.object["name"] == "After"
    ));
    assert!(matches!(
        third,
        ModelEvent::PostUpdate(event) if event.object["name"] == "After"
    ));

    let audit_first = timeout(Duration::from_secs(1), audit_events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        audit_first,
        ModelEvent::PostSave(event) if event.created && event.object["name"] == "Before"
    ));

    let task_event = timeout(Duration::from_secs(1), task_events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        task_event,
        ModelEvent::PostSave(event) if event.created && event.object["title"] == "Task event"
    ));
}

#[tokio::test]
async fn lagging_signal_subscribers_do_not_block_writes() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();
    let mut events = db.signals().subscribe::<User>();

    for index in 0..1025 {
        User::objects(&db)
            .create()
            .set("email", format!("lag-{index}@example.com"))
            .set("name", format!("Lag {index}"))
            .execute()
            .await
            .unwrap();
    }

    assert!(matches!(
        events.recv().await,
        Err(broadcast::error::RecvError::Lagged(_))
    ));
}

#[tokio::test]
async fn sqlite_crud_flow() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();

    let user = User::objects(&db)
        .create()
        .set("email", "alice@example.com")
        .set("name", "Alice")
        .set("is_active", true)
        .execute()
        .await
        .unwrap();

    assert_eq!(user.id, 1);
    assert_eq!(user.email, "alice@example.com");
    assert!(user.is_active);

    let fetched = User::objects(&db).get(user.id).await.unwrap();
    assert_eq!(fetched.name, "Alice");

    let typed = User::objects(&db)
        .query()
        .eq(UserFields::NAME, "Alice")
        .all()
        .await
        .unwrap();
    assert_eq!(typed.len(), 1);

    let all = User::objects(&db).all().await.unwrap();
    assert_eq!(all.len(), 1);

    let updated = User::objects(&db)
        .update_fields(user.id)
        .set("name", "Alicia")
        .set("is_active", false)
        .execute()
        .await
        .unwrap();
    assert_eq!(updated.name, "Alicia");
    assert!(!updated.is_active);

    let mut changed = User::objects(&db).get(user.id).await.unwrap();
    changed.name = "Alice Saved".to_string();
    changed.is_active = true;
    let saved = changed.save(&db).await.unwrap();
    assert_eq!(saved.name, "Alice Saved");
    assert!(saved.is_active);

    User::objects(&db).delete(user.id).await.unwrap();
    let all = User::objects(&db).all().await.unwrap();
    assert!(all.is_empty());
}

#[tokio::test]
async fn choice_field_roundtrips_and_enforces_values() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<ChoiceTask>().await.unwrap();

    let task = ChoiceTask::objects(&db)
        .create()
        .set("status", SqliteValue::String("in_progress".to_string()))
        .execute()
        .await
        .unwrap();
    assert_eq!(task.status, TaskStatus::InProgress);
    assert_eq!(TaskStatus::values(), &["new", "in_progress", "done"]);
    assert_eq!(ChoiceTaskFields::STATUS.db_name(), "status");
    assert!(
        ChoiceTask::objects(&db)
            .create()
            .set("status", SqliteValue::String("invalid".to_string()))
            .execute()
            .await
            .is_err()
    );
}

#[test]
fn generates_create_table_sql() {
    let sql = create_table_sql::<User>();

    assert!(sql.contains("CREATE TABLE IF NOT EXISTS users"));
    assert!(sql.contains("id INTEGER PRIMARY KEY AUTOINCREMENT"));
    assert!(sql.contains("email TEXT NOT NULL UNIQUE"));
    assert!(sql.contains("is_active BOOLEAN NOT NULL DEFAULT false"));

    let task_sql = create_table_sql::<Task>();
    assert!(task_sql.contains("created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP"));
    assert!(task_sql.contains("updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP"));

    let json_task_sql = create_table_sql::<JsonTask>();
    assert!(json_task_sql.contains("metadata TEXT NOT NULL"));
    assert!(json_task_sql.contains("optional_metadata TEXT"));
}

#[tokio::test]
async fn timestamp_fields_are_managed_by_orm() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<Task>().await.unwrap();

    let task = Task::objects(&db)
        .create()
        .set("title", "First")
        .execute()
        .await
        .unwrap();
    assert_eq!(task.title, "First");
    assert_eq!(task.created_at, task.updated_at);

    assert!(
        Task::objects(&db)
            .create()
            .set("title", "Readonly Create")
            .set("created_at", task.created_at)
            .execute()
            .await
            .is_err()
    );
    assert!(
        Task::objects(&db)
            .update_fields(task.id)
            .set("updated_at", task.updated_at)
            .execute()
            .await
            .is_err()
    );

    std::thread::sleep(std::time::Duration::from_millis(1100));
    let updated = Task::objects(&db)
        .update_fields(task.id)
        .set("title", "Updated")
        .execute()
        .await
        .unwrap();
    assert_eq!(updated.created_at, task.created_at);
    assert!(updated.updated_at > task.updated_at);

    std::thread::sleep(std::time::Duration::from_millis(1100));
    let mut changed = updated.clone();
    changed.title = "Saved".to_string();
    let saved = changed.save(&db).await.unwrap();
    assert_eq!(saved.created_at, task.created_at);
    assert!(saved.updated_at > updated.updated_at);
}

#[tokio::test]
async fn json_fields_roundtrip_update_and_save() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<JsonTask>().await.unwrap();

    let metadata = json!({
        "priority": "high",
        "tags": ["backend", "orm"],
        "nested": { "done": false }
    });
    let task = JsonTask::objects(&db)
        .create()
        .set("title", "JSON")
        .set("metadata", metadata.clone())
        .set_null("optional_metadata")
        .execute()
        .await
        .unwrap();
    assert_eq!(task.metadata, metadata);
    assert_eq!(task.optional_metadata, None);

    let fetched = JsonTask::objects(&db).get(task.id).await.unwrap();
    assert_eq!(fetched.metadata["priority"], "high");

    let optional_metadata = json!(["a", "b", 3]);
    let updated = JsonTask::objects(&db)
        .update_fields(task.id)
        .set("metadata", json!({ "done": true }))
        .set("optional_metadata", optional_metadata.clone())
        .execute()
        .await
        .unwrap();
    assert_eq!(updated.metadata, json!({ "done": true }));
    assert_eq!(updated.optional_metadata, Some(optional_metadata));

    let mut changed = updated.clone();
    changed.metadata = json!({ "saved": true });
    changed.optional_metadata = None;
    let saved = changed.save(&db).await.unwrap();
    assert_eq!(saved.metadata, json!({ "saved": true }));
    assert_eq!(saved.optional_metadata, None);
}

#[tokio::test]
async fn applies_migration_files_without_exposing_sqlx() {
    let migrations_dir = std::env::temp_dir().join(format!(
        "che_orm_migrations_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&migrations_dir).unwrap();
    std::fs::write(
        migrations_dir.join("0001_initial.sql"),
        create_table_sql::<User>(),
    )
    .unwrap();

    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    let applied = db.apply_migrations_dir(&migrations_dir).await.unwrap();
    assert_eq!(applied, vec!["0001_initial.sql"]);

    let user = User::objects(&db)
        .create()
        .set("email", "migration@example.com")
        .set("name", "Migration")
        .set("is_active", true)
        .execute()
        .await
        .unwrap();
    assert_eq!(user.id, 1);

    std::fs::remove_dir_all(migrations_dir).unwrap();
}

#[tokio::test]
async fn migration_checksums_reject_modified_applied_files() {
    let migrations_dir = std::env::temp_dir().join(format!(
        "che_orm_checksum_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&migrations_dir).unwrap();
    let migration_path = migrations_dir.join("0001_initial.sql");
    std::fs::write(
        &migration_path,
        "CREATE TABLE checksum_test (id INTEGER PRIMARY KEY);",
    )
    .unwrap();

    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.apply_migrations_dir(&migrations_dir).await.unwrap();
    let status = db.migration_status(&migrations_dir).await.unwrap();
    assert!(status[0].applied);
    assert!(status[0].checksum.is_some());

    std::fs::write(
        &migration_path,
        "CREATE TABLE checksum_test (id INTEGER PRIMARY KEY, value TEXT);",
    )
    .unwrap();
    let status = db.migration_status(&migrations_dir).await.unwrap();
    assert!(status[0].checksum_mismatch);
    assert!(db.apply_migrations_dir(&migrations_dir).await.is_err());

    std::fs::remove_dir_all(migrations_dir).unwrap();
}

#[tokio::test]
async fn migration_sql_parser_handles_strings_and_triggers() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.apply_sql(
        r#"
        CREATE TABLE trigger_source (value TEXT);
        CREATE TABLE trigger_log (value TEXT);
        CREATE TRIGGER copy_trigger AFTER INSERT ON trigger_source FOR EACH ROW
        BEGIN
            INSERT INTO trigger_log(value) VALUES ('contains;semicolon');
        END -- trigger comment
        ;
        INSERT INTO trigger_source(value) VALUES ('source');
        "#,
    )
    .await
    .unwrap();

    let value: String = sqlx::query_scalar("SELECT value FROM trigger_log")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(value, "contains;semicolon");
}

#[tokio::test]
async fn update_fields_rejects_empty_and_readonly_updates() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();

    let user = User::objects(&db)
        .create()
        .set("email", "readonly@example.com")
        .set("name", "Readonly")
        .set("is_active", true)
        .execute()
        .await
        .unwrap();

    assert!(
        User::objects(&db)
            .update_fields(user.id)
            .execute()
            .await
            .is_err()
    );
    assert!(
        User::objects(&db)
            .update_fields(user.id)
            .set("id", 2_i64)
            .execute()
            .await
            .is_err()
    );
}

#[tokio::test]
async fn create_builder_uses_defaults_and_rejects_readonly_fields() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();

    let user = User::objects(&db)
        .create()
        .set("email", "default@example.com")
        .set("name", "Default")
        .execute()
        .await
        .unwrap();
    assert!(!user.is_active);

    assert!(
        User::objects(&db)
            .create()
            .set("id", 42_i64)
            .set("email", "readonly-create@example.com")
            .set("name", "Readonly Create")
            .execute()
            .await
            .is_err()
    );
}

#[tokio::test]
async fn query_builder_filters_orders_and_limits() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();

    for (email, name, is_active) in [
        ("alice@example.com", "Alice", true),
        ("alicia@example.com", "Alicia", true),
        ("bob@example.com", "Bob", false),
    ] {
        User::objects(&db)
            .create()
            .set("email", email)
            .set("name", name)
            .set("is_active", is_active)
            .execute()
            .await
            .unwrap();
    }

    let users = User::objects(&db)
        .query()
        .contains(UserFields::NAME, "Ali")
        .eq(UserFields::IS_ACTIVE, true)
        .order_by_desc(UserFields::ID)
        .limit(1)
        .all()
        .await
        .unwrap();

    assert_eq!(users.len(), 1);
    assert_eq!(users[0].name, "Alicia");
}

#[tokio::test]
async fn query_supports_typed_distinct_projections() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();
    for (email, name) in [
        ("projection-one@example.com", "Same"),
        ("projection-two@example.com", "Same"),
        ("projection-three@example.com", "Other"),
    ] {
        User::objects(&db)
            .create()
            .set("email", email)
            .set("name", name)
            .set("is_active", true)
            .execute()
            .await
            .unwrap();
    }

    let rows = User::objects(&db)
        .query()
        .values([UserFields::NAME])
        .unwrap()
        .distinct()
        .all()
        .await
        .unwrap();
    let names = rows
        .iter()
        .map(|row| match row.get("name").unwrap() {
            SqliteValue::String(value) => value.clone(),
            value => panic!("unexpected projection value: {value:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["Same", "Other"]);
}

#[tokio::test]
async fn query_supports_typed_tuple_projections() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();
    User::objects(&db)
        .create()
        .set("email", "typed-projection@example.com")
        .set("name", "Typed")
        .set("is_active", true)
        .execute()
        .await
        .unwrap();

    let rows: Vec<(i64, String)> = User::objects(&db)
        .query()
        .select((UserFields::ID, UserFields::NAME))
        .unwrap()
        .all()
        .await
        .unwrap();
    assert_eq!(rows, vec![(1, "Typed".to_string())]);
}

#[tokio::test]
async fn typed_queries_support_all_scalar_field_types() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<ScalarTypes>().await.unwrap();
    ScalarTypes::objects(&db)
        .create()
        .set("small", 7_i32)
        .set("unsigned", 9_u32)
        .set("ratio", 1.5_f32)
        .execute()
        .await
        .unwrap();

    assert!(
        ScalarTypes::objects(&db)
            .query()
            .filter(ScalarTypesFields::SMALL.eq(7_i32))
            .first()
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        ScalarTypes::objects(&db)
            .query()
            .sum(ScalarTypesFields::SMALL)
            .await
            .unwrap(),
        Some(7_i32)
    );
    let projected: (u32, f32) = ScalarTypes::objects(&db)
        .query()
        .select((ScalarTypesFields::UNSIGNED, ScalarTypesFields::RATIO))
        .unwrap()
        .all()
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(projected, (9_u32, 1.5_f32));
}

#[tokio::test]
async fn typed_projection_decodes_choice_values() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<ChoiceTask>().await.unwrap();
    ChoiceTask::objects(&db)
        .create()
        .set("status", "in_progress")
        .execute()
        .await
        .unwrap();

    let status: TaskStatus = ChoiceTask::objects(&db)
        .query()
        .select(ChoiceTaskFields::STATUS)
        .unwrap()
        .all()
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(status, TaskStatus::InProgress);
}

#[tokio::test]
async fn grouped_queries_support_having_and_count_annotation() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();
    for (email, active) in [
        ("group-one@example.com", true),
        ("group-two@example.com", true),
        ("group-three@example.com", false),
    ] {
        User::objects(&db)
            .create()
            .set("email", email)
            .set("name", "Grouped")
            .set("is_active", active)
            .execute()
            .await
            .unwrap();
    }

    let total = AnnotationField::<i64>::new("total");
    let repeated = AnnotationField::<i64>::new("repeated");
    let rows = User::objects(&db)
        .query()
        .values([UserFields::IS_ACTIVE])
        .unwrap()
        .group_by()
        .annotate_count_field(&total, UserFields::ID)
        .unwrap()
        .annotate_count_field(&repeated, UserFields::ID)
        .unwrap()
        .having(UserFields::IS_ACTIVE.eq(true))
        .having_annotation_field(total.clone().gte(2_i64))
        .all_typed((UserFields::IS_ACTIVE, total, repeated))
        .await
        .unwrap();
    assert_eq!(rows, vec![(true, 2_i64, 2_i64)]);
}

#[tokio::test]
async fn grouped_queries_reject_invalid_annotation_specs() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();
    User::objects(&db)
        .create()
        .set("email", "annotation-check@example.com")
        .set("name", "Check")
        .set("is_active", true)
        .execute()
        .await
        .unwrap();

    let collision = User::objects(&db)
        .query()
        .values([UserFields::IS_ACTIVE])
        .unwrap()
        .group_by()
        .annotate_count("is_active", UserFields::ID);
    assert!(matches!(
        collision,
        Err(che_orm::Error::InvalidAnnotation(_))
    ));

    let duplicate = User::objects(&db)
        .query()
        .values([UserFields::IS_ACTIVE])
        .unwrap()
        .group_by()
        .annotate_count("total", UserFields::ID)
        .unwrap()
        .annotate_count("total", UserFields::ID);
    assert!(matches!(
        duplicate,
        Err(che_orm::Error::InvalidAnnotation(_))
    ));

    let wrong_type = AnnotationField::<String>::new("total");
    let error = User::objects(&db)
        .query()
        .values([UserFields::IS_ACTIVE])
        .unwrap()
        .group_by()
        .annotate_count("total", UserFields::ID)
        .unwrap()
        .all_typed((UserFields::IS_ACTIVE, wrong_type))
        .await
        .unwrap_err();
    assert!(matches!(error, che_orm::Error::InvalidAnnotation(_)));

    let total = AnnotationField::<i64>::new("total");
    let missing = AnnotationField::<i64>::new("missing");
    let error = User::objects(&db)
        .query()
        .values([UserFields::IS_ACTIVE])
        .unwrap()
        .group_by()
        .annotate_count_field(&total, UserFields::ID)
        .unwrap()
        .all_typed((UserFields::NAME, missing))
        .await
        .unwrap_err();
    assert!(matches!(error, che_orm::Error::InvalidAnnotation(_)));
}

#[tokio::test]
async fn query_supports_q_in_null_first_and_multiple_orderings() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();

    for (email, name, is_active) in [
        ("alice@example.com", "Alex", true),
        ("bob@example.com", "Alex", false),
        ("carol@example.com", "Carol", true),
    ] {
        User::objects(&db)
            .create()
            .set("email", email)
            .set("name", name)
            .set("is_active", is_active)
            .execute()
            .await
            .unwrap();
    }

    let users = User::objects(&db)
        .query()
        .filter(
            Q::from(UserFields::NAME.eq("Alex"))
                .or(UserFields::ID.in_values([3_i64]))
                .and(UserFields::IS_ACTIVE.eq(true).not()),
        )
        .order_by(UserFields::NAME)
        .order_by_desc(UserFields::ID)
        .all()
        .await
        .unwrap();
    assert_eq!(
        users.iter().map(|user| user.id).collect::<Vec<_>>(),
        vec![2]
    );

    let first = User::objects(&db)
        .query()
        .filter(UserFields::ID.in_values([1_i64, 2]))
        .order_by_desc(UserFields::ID)
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.id, 2);
    assert!(
        User::objects(&db)
            .query()
            .filter(UserFields::ID.in_values(Vec::<i64>::new()))
            .first()
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn query_supports_raw_range_filters_and_ordering() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();

    for (email, name) in [
        ("a@example.com", "A"),
        ("b@example.com", "B"),
        ("c@example.com", "C"),
    ] {
        User::objects(&db)
            .create()
            .set("email", email)
            .set("name", name)
            .set("is_active", true)
            .execute()
            .await
            .unwrap();
    }

    let users = User::objects(&db)
        .query()
        .gte_raw("id", 2_i64)
        .lt_raw("id", 4_i64)
        .order_by_raw("id", true)
        .all()
        .await
        .unwrap();
    assert_eq!(
        users.iter().map(|user| user.id).collect::<Vec<_>>(),
        vec![3, 2]
    );
}

#[tokio::test]
async fn update_one_returning_updates_only_the_first_match() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();

    for name in ["first", "second"] {
        User::objects(&db)
            .create()
            .set("email", format!("{name}@example.com"))
            .set("name", name)
            .set("is_active", true)
            .execute()
            .await
            .unwrap();
    }

    let updated = User::objects(&db)
        .query()
        .filter(UserFields::IS_ACTIVE.eq(true))
        .order_by(UserFields::ID)
        .update_one_returning([("name", "claimed")])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.id, 1);

    let claimed = User::objects(&db)
        .query()
        .filter(UserFields::NAME.eq("claimed"))
        .count()
        .await
        .unwrap();
    let remaining = User::objects(&db)
        .query()
        .filter(UserFields::IS_ACTIVE.eq(true))
        .count()
        .await
        .unwrap();
    assert_eq!(claimed, 1);
    assert_eq!(remaining, 2);
}

#[tokio::test]
async fn update_one_returning_handles_no_match_and_descending_order() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();
    for name in ["first", "second"] {
        User::objects(&db)
            .create()
            .set("email", format!("{name}-order@example.com"))
            .set("name", name)
            .set("is_active", true)
            .execute()
            .await
            .unwrap();
    }

    assert!(
        User::objects(&db)
            .query()
            .filter(UserFields::NAME.eq("missing"))
            .update_one_returning([("name", "never")])
            .await
            .unwrap()
            .is_none()
    );
    let updated = User::objects(&db)
        .query()
        .filter(UserFields::IS_ACTIVE.eq(true))
        .order_by_desc(UserFields::ID)
        .update_one_returning([("name", "descending")])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.id, 2);
}

#[tokio::test]
async fn concurrent_claims_return_distinct_rows() {
    let path = std::env::temp_dir().join(format!(
        "che_orm_claims_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::File::create(&path).unwrap();
    let url = format!("sqlite://{}", path.display());
    let setup = SqliteBackend::connect(&url).await.unwrap();
    setup.create_table::<User>().await.unwrap();
    for index in 0..2 {
        User::objects(&setup)
            .create()
            .set("email", format!("claim-{index}@example.com"))
            .set("name", format!("claim-{index}"))
            .set("is_active", true)
            .execute()
            .await
            .unwrap();
    }
    let first = SqliteBackend::connect(&url).await.unwrap();
    let second = SqliteBackend::connect(&url).await.unwrap();
    let (left, right) = tokio::join!(
        User::objects(&first)
            .query()
            .filter(UserFields::IS_ACTIVE.eq(true))
            .claim_next_returning([("is_active", false)]),
        User::objects(&second)
            .query()
            .filter(UserFields::IS_ACTIVE.eq(true))
            .claim_next_returning([("is_active", false)])
    );
    let left = left.unwrap().unwrap();
    let right = right.unwrap().unwrap();
    assert_ne!(left.id, right.id);
    assert_eq!(
        User::objects(&setup)
            .query()
            .filter(UserFields::IS_ACTIVE.eq(true))
            .count()
            .await
            .unwrap(),
        0
    );
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn query_supports_null_predicates_and_numeric_aggregates() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<Metric>().await.unwrap();

    for (score, value) in [(Some(10_i64), 1.5), (None, 2.5), (Some(30_i64), 3.5)] {
        let mut create = Metric::objects(&db).create().set("value", value);
        create = match score {
            Some(score) => create.set("score", score),
            None => create.set_null("score"),
        };
        create.execute().await.unwrap();
    }

    assert_eq!(
        Metric::objects(&db)
            .query()
            .filter(MetricFields::SCORE.is_null())
            .count()
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        Metric::objects(&db)
            .query()
            .filter(MetricFields::SCORE.is_not_null())
            .sum(MetricFields::SCORE)
            .await
            .unwrap(),
        Some(40_i64)
    );
    assert_eq!(
        Metric::objects(&db)
            .query()
            .avg(MetricFields::VALUE)
            .await
            .unwrap(),
        Some(2.5)
    );
    assert_eq!(
        Metric::objects(&db)
            .query()
            .min(MetricFields::VALUE)
            .await
            .unwrap(),
        Some(1.5)
    );
    assert_eq!(
        Metric::objects(&db)
            .query()
            .max(MetricFields::VALUE)
            .await
            .unwrap(),
        Some(3.5)
    );
    assert_eq!(
        Metric::objects(&db)
            .query()
            .filter(MetricFields::ID.gt(100_i64))
            .sum(MetricFields::VALUE)
            .await
            .unwrap(),
        None
    );
}
