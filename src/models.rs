use crate::Model;

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
}

impl User {
    pub fn new(name: String) -> Self {
        Self {
            id: 0,
            email: String::new(),
            name,
            is_active: true,
        }
    }
}
