use std::sync::Arc;

use che_orm2::Database;

#[derive(Clone)]
pub struct RestState {
    database: Arc<Database>,
}

impl RestState {
    pub fn new(database: Database) -> Self {
        Self {
            database: Arc::new(database),
        }
    }

    pub fn database(&self) -> &Database {
        &self.database
    }
}
