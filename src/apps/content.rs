use crate::Model;

#[derive(Debug, Model)]
#[orm(table = "posts", index("user_id"))]
pub struct Post {
    #[orm(primary_key)]
    pub id: i64,
    #[orm(foreign_key = crate::apps::accounts::User, on_delete = "cascade")]
    pub user_id: i64,
    pub title: String,
}

pub struct App;

impl crate::AppConfig for App {
    fn name() -> &'static str {
        "content"
    }

    fn schema() -> crate::SchemaSet {
        crate::SchemaSet::new().model::<Post>()
    }
}
