use std::marker::PhantomData;

#[derive(Debug, Clone, Copy)]
pub struct ModelField<M, T> {
    db_name: &'static str,
    _marker: PhantomData<fn() -> (M, T)>,
}

impl<M, T> ModelField<M, T> {
    pub const fn new(db_name: &'static str) -> Self {
        Self {
            db_name,
            _marker: PhantomData,
        }
    }

    pub const fn db_name(&self) -> &'static str {
        self.db_name
    }
}

#[derive(Debug)]
pub struct User {
    id: u64,
    name: String,
}
impl User {
    pub fn new(name: String) -> Self {
        Self { id: 0, name }
    }
}
impl User {
    pub const ID: ModelField<Self, i64> = ModelField::new("id");

    pub const NAME: ModelField<Self, String> = ModelField::new("name");
}
