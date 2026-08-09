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

#[derive(Debug, Clone, Model)]
#[model(table = "cascade_posts")]
struct CascadePost {
    #[field(primary_key)]
    id: i64,
    #[field(foreign_key = User, on_delete = Cascade)]
    user_id: i64,
    title: String,
}

#[derive(Debug, Clone, Model)]
#[model(table = "nullable_posts")]
struct NullablePost {
    #[field(primary_key)]
    id: i64,
    #[field(foreign_key = User, on_delete = SetNull)]
    user_id: Option<i64>,
}

#[derive(Debug, Clone, Model)]
#[model(table = "restricted_posts")]
struct RestrictedPost {
    #[field(primary_key)]
    id: i64,
    #[field(foreign_key = User, on_delete = Restrict)]
    user_id: i64,
}

#[derive(Debug, Clone, Model)]
#[model(table = "defaulted_posts")]
struct DefaultedPost {
    #[field(primary_key)]
    id: i64,
    #[field(foreign_key = User, on_delete = SetDefault, default = 1)]
    user_id: i64,
}

#[tokio::test]
async fn loads_related_and_reverse_related_objects() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();
    db.create_table::<Post>().await.unwrap();

    let user = User::objects(&db)
        .create()
        .set("name", "Alice")
        .execute()
        .await
        .unwrap();
    let post = Post::objects(&db)
        .create()
        .set("user_id", user.id)
        .set("title", "First post")
        .execute()
        .await
        .unwrap();

    let author = Post::objects(&db)
        .get_related::<User>(post.user_id)
        .await
        .unwrap();
    assert_eq!(author.name, "Alice");

    let descriptor_author = PostRelations::USER
        .get(&db, post.user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(descriptor_author.name, "Alice");

    let posts = PostRelations::USER
        .reverse()
        .query(&db, user.id)
        .all()
        .await
        .unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].title, "First post");

    let selected = Post::objects(&db)
        .query()
        .select_related(PostRelations::USER)
        .all()
        .await
        .unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].1.as_ref().unwrap().name, "Alice");

    let prefetched = User::objects(&db)
        .query()
        .prefetch_related(PostRelations::USER.reverse())
        .all()
        .await
        .unwrap();
    assert_eq!(prefetched.related_for(&user).len(), 1);
    assert_eq!(prefetched.related_for(&user)[0].title, "First post");
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
    let sql = create_table_sql::<Post>();
    assert!(sql.contains("user_id INTEGER NOT NULL REFERENCES relation_users(id)"));
}

#[tokio::test]
async fn foreign_key_actions_are_rendered_and_enforced() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();
    db.create_table::<CascadePost>().await.unwrap();
    let user = User::objects(&db)
        .create()
        .set("name", "Alice")
        .execute()
        .await
        .unwrap();
    CascadePost::objects(&db)
        .create()
        .set("user_id", user.id)
        .set("title", "First")
        .execute()
        .await
        .unwrap();

    let schema = Schema::from_model::<CascadePost>();
    let foreign_key = schema.models[0].fields[1].foreign_key.as_ref().unwrap();
    assert_eq!(foreign_key.on_delete, che_orm::ForeignKeyAction::Cascade);
    assert!(create_table_sql::<CascadePost>().contains("ON DELETE CASCADE"));

    User::objects(&db).delete(user.id).await.unwrap();
    assert_eq!(CascadePost::objects(&db).all().await.unwrap().len(), 0);
}

#[tokio::test]
async fn set_null_action_is_rendered_and_enforced() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();
    db.create_table::<NullablePost>().await.unwrap();
    let user = User::objects(&db)
        .create()
        .set("name", "Alice")
        .execute()
        .await
        .unwrap();
    let post = NullablePost::objects(&db)
        .create()
        .set("user_id", user.id)
        .execute()
        .await
        .unwrap();

    assert!(create_table_sql::<NullablePost>().contains("ON DELETE SET NULL"));
    User::objects(&db).delete(user.id).await.unwrap();
    let post = NullablePost::objects(&db).get(post.id).await.unwrap();
    assert_eq!(post.user_id, None);
    assert!(
        NullablePostRelations::USER
            .get_optional(&db, post.user_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn restrict_and_set_default_actions_are_enforced() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();
    db.create_table::<RestrictedPost>().await.unwrap();
    db.create_table::<DefaultedPost>().await.unwrap();
    let fallback = User::objects(&db)
        .create()
        .set("name", "Fallback")
        .execute()
        .await
        .unwrap();
    let user = User::objects(&db)
        .create()
        .set("name", "Alice")
        .execute()
        .await
        .unwrap();
    RestrictedPost::objects(&db)
        .create()
        .set("user_id", user.id)
        .execute()
        .await
        .unwrap();
    assert!(create_table_sql::<RestrictedPost>().contains("ON DELETE RESTRICT"));
    assert!(User::objects(&db).delete(user.id).await.is_err());

    RestrictedPost::objects(&db)
        .query()
        .filter(RestrictedPostFields::USER_ID.eq(user.id))
        .update_one_returning([("user_id", fallback.id)])
        .await
        .unwrap();
    let post = DefaultedPost::objects(&db)
        .create()
        .set("user_id", user.id)
        .execute()
        .await
        .unwrap();
    assert!(create_table_sql::<DefaultedPost>().contains("ON DELETE SET DEFAULT"));
    User::objects(&db).delete(user.id).await.unwrap();
    assert_eq!(
        DefaultedPost::objects(&db)
            .get(post.id)
            .await
            .unwrap()
            .user_id,
        fallback.id
    );
}

#[tokio::test]
async fn prefetch_related_chunks_large_parent_sets() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();
    db.create_table::<Post>().await.unwrap();
    for index in 0..901_i64 {
        let user = User::objects(&db)
            .create()
            .set("name", format!("User {index}"))
            .execute()
            .await
            .unwrap();
        Post::objects(&db)
            .create()
            .set("user_id", user.id)
            .set("title", format!("Post {index}"))
            .execute()
            .await
            .unwrap();
    }

    let prefetched = User::objects(&db)
        .query()
        .prefetch_related(PostRelations::USER.reverse())
        .all()
        .await
        .unwrap();
    assert_eq!(prefetched.parents.len(), 901);
    assert_eq!(prefetched.related_for(&prefetched.parents[900]).len(), 1);
}

#[tokio::test]
async fn eager_loading_rejects_invalid_relation_descriptors() {
    let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    db.create_table::<User>().await.unwrap();
    db.create_table::<Post>().await.unwrap();

    let error = Post::objects(&db)
        .query()
        .select_related(che_orm::BelongsTo::<Post, User>::new("title"))
        .all()
        .await
        .unwrap_err();
    assert!(matches!(error, che_orm::Error::InvalidRelation(_)));

    let error = User::objects(&db)
        .query()
        .prefetch_related(che_orm::HasMany::<User, Post>::new("title"))
        .all()
        .await
        .unwrap_err();
    assert!(matches!(error, che_orm::Error::InvalidRelation(_)));
}
