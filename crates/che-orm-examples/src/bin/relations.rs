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
        .create()
        .set("name", "Alice")
        .execute()
        .await?;

    let post = Post::objects(&db)
        .create()
        .set("author_id", author.id)
        .set("title", "Building a Django-like ORM in Rust")
        .execute()
        .await?;

    let loaded_author = PostRelations::AUTHOR.get(&db, post.author_id).await?;
    println!("post author: {loaded_author:?}");

    let author_posts = PostRelations::AUTHOR
        .reverse()
        .query(&db, author.id)
        .all()
        .await?;
    println!("author posts: {author_posts:?}");

    Ok(())
}
