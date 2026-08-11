#![allow(dead_code)]

use che_orm::{Database, Model};

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
    let db = Database::connect("sqlite::memory:").await?;
    db.create_table::<Author>().await?;
    db.create_table::<Post>().await?;

    let author = db.create::<Author>().set("name", "Alice").execute().await?;

    let post = db
        .create::<Post>()
        .set("author_id", author.id)
        .set("title", "Building a Django-like ORM in Rust")
        .execute()
        .await?;

    let loaded_author = PostRelations::AUTHOR
        .get(db.as_sqlite(), post.author_id)
        .await?;
    println!("post author: {loaded_author:?}");

    let author_posts = PostRelations::AUTHOR
        .reverse()
        .query(db.as_sqlite(), author.id)
        .all()
        .await?;
    println!("author posts: {author_posts:?}");

    Ok(())
}
