use orm::{Model, ModelSerializer};
use time::OffsetDateTime;

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
    pub name: String,
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
