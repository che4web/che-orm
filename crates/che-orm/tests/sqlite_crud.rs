use std::time::{SystemTime, UNIX_EPOCH};

use che_orm::{Model, SqliteBackend, create_table_sql};

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

#[test]
fn generates_create_table_sql() {
    let sql = create_table_sql::<User>();

    assert!(sql.contains("CREATE TABLE IF NOT EXISTS users"));
    assert!(sql.contains("id INTEGER PRIMARY KEY AUTOINCREMENT"));
    assert!(sql.contains("email TEXT NOT NULL UNIQUE"));
    assert!(sql.contains("is_active BOOLEAN NOT NULL DEFAULT false"));
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
        .contains("name", "Ali")
        .eq("is_active", true)
        .order_by("-id")
        .limit(1)
        .all()
        .await
        .unwrap();

    assert_eq!(users.len(), 1);
    assert_eq!(users[0].name, "Alicia");
}
