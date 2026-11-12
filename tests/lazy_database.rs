use che_orm::Database;

#[test]
fn lazy_connections_validate_pool_size() {
    let url = if cfg!(feature = "postgres") {
        "postgresql://127.0.0.1:1/unreachable?sslmode=disable"
    } else {
        ":memory:"
    };
    assert!(Database::connect_lazy_with_pool_size(url, 0).is_err());
    assert!(Database::connect_lazy(url).is_ok());
}

#[tokio::test]
#[cfg(feature = "sqlite")]
async fn lazy_sqlite_does_not_create_a_file_until_first_use() {
    let path =
        std::env::temp_dir().join(format!("che-orm-lazy-{}.sqlite", che_orm::Uuid::now_v7()));
    let database = Database::connect_lazy(path.to_string_lossy().into_owned()).unwrap();
    assert!(!path.exists());
    database.query_sql("SELECT 1", vec![]).await.unwrap();
    assert!(path.exists());
    drop(database);
    std::fs::remove_file(path).unwrap();
}
