use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use che_orm::{Database, FilePath, FileStorage, LocalFileStorage, Model};

#[derive(Debug, Clone, Model)]
#[model(table = "file_assets")]
struct Asset {
    #[field(primary_key)]
    id: i64,
    path: FilePath,
    optional_path: Option<FilePath>,
}

#[test]
fn file_paths_reject_traversal() {
    assert!(FilePath::new("../secret.txt").is_err());
    assert!(FilePath::new("/tmp/secret.txt").is_err());
    assert!(FilePath::new(r"..\secret.txt").is_err());
    assert!(FilePath::new("safe/./file.txt").is_err());
    assert!(FilePath::new("safe/file.txt").is_ok());
}

#[test]
fn local_storage_roundtrip() {
    let root = std::env::temp_dir().join(format!(
        "che-orm-files-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let storage = LocalFileStorage::new(&root);
    let path = storage.store(b"hello", Some("txt")).unwrap();
    assert!(storage.exists(&path).unwrap());
    assert_eq!(storage.read(&path).unwrap(), b"hello");
    storage.delete(&path).unwrap();
    assert!(!storage.exists(&path).unwrap());
    assert!(storage.store(b"x", Some("../txt")).is_err());
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn file_path_model_roundtrips() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.create_table::<Asset>().await.unwrap();
    let path = FilePath::new("aa/bb/report.txt").unwrap();
    let asset = db
        .create::<Asset>()
        .set(AssetFields::PATH, path.clone())
        .execute()
        .await
        .unwrap();
    assert_eq!(asset.path, path);
    assert!(asset.optional_path.is_none());
    assert_eq!(
        db.get::<Asset>(asset.id).await.unwrap().path,
        FilePath::new("aa/bb/report.txt").unwrap()
    );
}

#[tokio::test]
async fn dynamic_writes_reject_unsafe_file_paths() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.create_table::<Asset>().await.unwrap();

    assert!(
        db.create::<Asset>()
            .set_value("path", "../secret.txt".into())
            .is_err()
    );
}

#[tokio::test]
async fn typed_projection_decodes_file_paths_and_nullable_values() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.create_table::<Asset>().await.unwrap();
    let path = FilePath::new("typed/report.txt").unwrap();
    let asset = db
        .create::<Asset>()
        .set(AssetFields::PATH, path.clone())
        .execute()
        .await
        .unwrap();

    let projected: FilePath = db
        .query::<Asset>()
        .filter(AssetFields::ID.eq(asset.id))
        .select(AssetFields::PATH)
        .unwrap()
        .all()
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(projected, path);

    let optional: Option<FilePath> = db
        .query::<Asset>()
        .filter(AssetFields::ID.eq(asset.id))
        .select(AssetFields::OPTIONAL_PATH.optional())
        .unwrap()
        .all()
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(optional, None);
}
