use che_orm2::Model;
use time::OffsetDateTime;

#[derive(Debug, Model)]
#[orm(table = "examples_users", index("name"))]
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

pub fn database_schema_sql() -> String {
    che_orm2::SqlCompiler::<che_orm2::SqliteDialect>::compile(
        &ExampleUser::create_table().into_ast(),
    )
    .sql
}
