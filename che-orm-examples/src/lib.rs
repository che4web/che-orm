use orm::{Model, ModelSerializer};
use std::path::Path;
use time::OffsetDateTime;

pub const DATABASE_PATH: &str = "app.db";

pub const fn database_path() -> &'static str {
    DATABASE_PATH
}

pub fn atlas_database_url() -> Result<String, String> {
    let path = database_path();
    if path.is_empty() {
        return Err("database path must not be empty".into());
    }
    if path == ":memory:" {
        return Err("in-memory SQLite cannot be used for Atlas migrations".into());
    }
    if Path::new(path).is_dir() {
        return Err(format!("database path points to a directory: {path}"));
    }
    Ok(format!("sqlite://{path}"))
}

#[allow(dead_code)]
#[derive(orm::Model)]
#[orm(table = "aliased_models")]
struct AliasedModel {
    #[orm(primary_key)]
    id: i64,
    name: String,
}

#[derive(Debug, Model)]
#[orm(table = "users", index("name"))]
pub struct ExampleUser {
    #[orm(primary_key)]
    pub id: i64,
    #[orm(unique)]
    pub email: String,
    #[orm(check = "length(name) > 0")]
    pub name: String,
    #[orm(default = "true")]
    pub is_active: bool,
    #[orm(auto_now_add)]
    pub created_at: OffsetDateTime,
    #[orm(auto_now)]
    pub updated_at: OffsetDateTime,
}

impl ExampleUser {
    pub fn new(email: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: 0,
            email: email.into(),
            name: name.into(),
            is_active: true,
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
        }
    }
}

#[derive(Debug, Model)]
#[orm(table = "posts", index("user_id"))]
pub struct ExamplePost {
    #[orm(primary_key)]
    pub id: i64,
    #[orm(foreign_key = ExampleUser, on_delete = "cascade")]
    pub user_id: i64,
    pub title: String,
}

pub struct AccountsApp;

impl orm::AppConfig for AccountsApp {
    fn name() -> &'static str {
        "accounts"
    }

    fn schema() -> orm::SchemaSet {
        orm::SchemaSet::new().model::<ExampleUser>()
    }
}

pub struct ContentApp;

impl orm::AppConfig for ContentApp {
    fn name() -> &'static str {
        "content"
    }

    fn schema() -> orm::SchemaSet {
        orm::SchemaSet::new().model::<ExamplePost>()
    }
}

pub fn registry() -> orm::AppRegistry {
    orm::AppRegistry::new()
        .register::<AccountsApp>()
        .register::<ContentApp>()
}

#[derive(ModelSerializer)]
#[serializer(model = ExampleUser)]
pub struct ExampleUserSerializer {
    #[serializer(read_only)]
    pub id: i64,
    pub email: String,
    pub name: String,
    pub is_active: bool,
}

#[derive(ModelSerializer)]
#[serializer(model = ExamplePost)]
pub struct ExamplePostSerializer {
    #[serializer(read_only)]
    pub id: i64,
    pub title: String,
}

#[derive(ModelSerializer)]
#[serializer(model = ExampleUser)]
pub struct ExampleUserWithPostsSerializer {
    pub id: i64,
    pub name: String,
    #[serializer(many = ExamplePost, relation = ExamplePostUserRelation)]
    pub posts: Vec<ExamplePostSerializer>,
}

#[derive(ModelSerializer)]
#[serializer(model = ExamplePost)]
pub struct ExamplePostWithUserSerializer {
    pub id: i64,
    pub title: String,
    #[serializer(one = ExampleUser, relation = ExamplePostUserRelation)]
    pub user: ExampleUserSerializer,
}

pub fn database_schema_sql() -> String {
    orm::SchemaSet::new()
        .model::<ExampleUser>()
        .model::<ExamplePost>()
        .to_sql::<orm::SqliteDialect>()
}
