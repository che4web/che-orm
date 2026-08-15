use crate::Model;
use time::OffsetDateTime;

#[derive(Debug, Model)]
#[orm(table = "users", unique("email"), index("name"))]
pub struct User {
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

impl User {
    pub fn new(name: String) -> Self {
        Self {
            id: 0,
            email: String::new(),
            name,
            is_active: true,
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
        }
    }
}

pub struct App;

impl crate::AppConfig for App {
    fn name() -> &'static str {
        "accounts"
    }

    fn schema() -> crate::SchemaSet {
        crate::SchemaSet::new().model::<User>()
    }
}
