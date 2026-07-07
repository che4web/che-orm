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
        .create(UserCreate {
            email: "alice@example.com".to_string(),
            name: "Alice".to_string(),
            is_active: true,
        })
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
        .create(UserCreate {
            email: "migration@example.com".to_string(),
            name: "Migration".to_string(),
            is_active: true,
        })
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
        .create(UserCreate {
            email: "readonly@example.com".to_string(),
            name: "Readonly".to_string(),
            is_active: true,
        })
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
