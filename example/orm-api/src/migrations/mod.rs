mod m0001_initial_models;

use orm::{Migration, MigrationError, MigrationGraph};

pub fn all() -> Vec<Migration> {
    vec![m0001_initial_models::migration()]
}

pub fn graph() -> Result<MigrationGraph, MigrationError> {
    MigrationGraph::new(all())
}
