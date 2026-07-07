#![allow(dead_code)]

use che_orm::{Model, SqliteBackend};

#[derive(Debug, Clone, Model)]
#[model(table = "authors")]
struct Author {
    #[field(primary_key)]
    id: i64,
    name: String,
}

#[derive(Debug, Clone, Model)]
#[model(table = "posts")]
struct Post {
    #[field(primary_key)]
    id: i64,

    #[field(foreign_key = Author)]
    author_id: i64,

    title: String,
}

#[tokio::main]
async fn main() -> che_orm::Result<()> {
    let db = SqliteBackend::connect("sqlite::memory:").await?;
    db.create_table::<Author>().await?;
    db.create_table::<Post>().await?;

    let author = Author::objects(&db)
        .create(AuthorCreate {
            name: "Alice".to_string(),
        })
        .await?;

    let post = Post::objects(&db)
        .create(PostCreate {
            author_id: author.id,
            title: "Building a Django-like ORM in Rust".to_string(),
        })
        .await?;

    let loaded_author = Post::objects(&db)
        .get_related::<Author>(post.author_id)
        .await?;
    println!("post author: {loaded_author:?}");

    let author_posts = Post::objects(&db)
        .filter_by_i64("author_id", author.id)
        .await?;
    println!("author posts: {author_posts:?}");

    Ok(())
}
