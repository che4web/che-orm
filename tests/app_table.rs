#[cfg(feature = "sqlite")]
use che_orm::Database;
use che_orm::{AppConfig, AppRegistry, Model, SchemaSet};

const APP_NAME: &str = "blog";

#[derive(Debug, Model)]
#[orm(app = APP_NAME)]
struct Post {
    #[orm(primary_key)]
    id: i64,
    title: String,
}

#[derive(Debug, Model)]
#[orm(app = "blog", index("post_id"))]
struct PostComment {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = Post, on_delete = "cascade")]
    post_id: i64,
}

#[derive(Debug, Model)]
#[orm(app = crate::APP_NAME, table = "legacy_posts")]
struct LegacyPost {
    #[orm(primary_key)]
    id: i64,
}

struct Blog;

impl AppConfig for Blog {
    fn name() -> &'static str {
        APP_NAME
    }

    fn schema() -> SchemaSet {
        SchemaSet::new().model::<Post>().model::<PostComment>()
    }
}

mod accounts {
    pub const APP_NAME: &str = "accounts";

    pub mod models {
        #[derive(Debug, che_orm::Model)]
        #[orm(app = super::APP_NAME)]
        pub struct User {
            #[orm(primary_key)]
            pub id: i64,
        }
    }
}

#[derive(Debug, Model)]
#[orm(app = crate::APP_NAME, sync)]
struct SyncedPost {
    #[orm(primary_key)]
    id: che_orm::Uuid,
}

#[derive(Debug, Model)]
#[orm(app = APP_NAME, sync, sync_name = "stable.post")]
struct StablePost {
    #[orm(primary_key)]
    id: che_orm::Uuid,
}

#[test]
fn app_prefix_is_used_by_metadata_relations_and_migration_state() {
    assert_eq!(Post::table_name(), "blog_post");
    assert_eq!(PostComment::table_name(), "blog_post_comment");
    assert_eq!(LegacyPost::table_name(), "legacy_posts");
    assert_eq!(accounts::models::User::table_name(), "accounts_user");
    assert_eq!(
        <SyncedPost as che_orm::SyncModel>::SYNC_NAME,
        "blog_synced_post"
    );
    assert_eq!(<StablePost as che_orm::SyncModel>::SYNC_NAME, "stable.post");
    let registry = AppRegistry::new().register::<Blog>();
    let sql = registry.to_sql::<che_orm::SqliteDialect>();
    assert!(sql.contains("REFERENCES blog_post(id)"));
    let state = che_orm::ProjectState::from_app_schemas(registry.app_schemas()).unwrap();
    assert!(state.tables().iter().any(|table| table.name == "blog_post"));
    assert!(
        state
            .tables()
            .iter()
            .any(|table| table.name == "blog_post_comment")
    );
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn prefixed_models_support_crud_and_foreign_keys() {
    let db = Database::connect(":memory:").await.unwrap();
    db.create_table::<Post>().await.unwrap();
    db.create_table::<PostComment>().await.unwrap();
    let post = db
        .create::<Post>()
        .set(Post::TITLE, "Hello")
        .execute()
        .await
        .unwrap();
    let comment = db
        .create::<PostComment>()
        .set(PostComment::POST_ID, post.id)
        .execute()
        .await
        .unwrap();
    assert_eq!(comment.post_id, post.id);
    let loaded = db.get::<Post>(post.id).await.unwrap().unwrap();
    assert_eq!(loaded.title, "Hello");
}
