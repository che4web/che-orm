mod m0001_initial;

use che_orm::{Migration, MigrationError, MigrationGraph};

pub fn all() -> Vec<Migration> {
    vec![m0001_initial::migration()]
}

pub fn graph() -> Result<MigrationGraph, MigrationError> {
    MigrationGraph::new(all())
}
