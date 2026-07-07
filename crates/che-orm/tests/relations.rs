#![allow(dead_code)]

use che_orm::{Model, Schema, SqliteBackend, create_table_sql};

#[derive(Debug, Clone, Model)]
#[model(table = "relation_users")]
struct User {
    #[field(primary_key)]
    id: i64,
    name: String,
}

#[derive(Debug, Clone, Model)]
#[model(table = "relation_posts")]
struct Post {
    #[field(primary_key)]
    id: i64,

    #[field(foreign_key = User)]
    user_id: i64,

    title: String,
}

#[tokio::test]
async fn loads_related_and_reverse_related_objects() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();
    db.create_table::<Post>().await.unwrap();

    let user = User::objects(&db)
        .create(UserCreate {
            name: "Alice".to_string(),
        })
        .await
        .unwrap();
    let post = Post::objects(&db)
        .create(PostCreate {
            user_id: user.id,
            title: "First post".to_string(),
        })
        .await
        .unwrap();

    let author = Post::objects(&db)
        .get_related::<User>(post.user_id)
        .await
        .unwrap();
    assert_eq!(author.name, "Alice");

    let posts = Post::objects(&db)
        .filter_by_i64("user_id", user.id)
        .await
        .unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].title, "First post");
}

#[test]
fn foreign_key_is_part_of_schema_and_create_table_sql() {
    let schema = Schema::from_model::<Post>();
    let user_id = schema.models[0]
        .fields
        .iter()
        .find(|field| field.name == "user_id")
        .unwrap();
    let foreign_key = user_id.foreign_key.as_ref().unwrap();

    assert_eq!(foreign_key.table, "relation_users");
    assert_eq!(foreign_key.column, "id");

    let sql = create_table_sql::<Post>();
    assert!(sql.contains("user_id INTEGER NOT NULL REFERENCES relation_users(id)"));
}
