#[path = "che_sync_migrations/mod.rs"]
pub(crate) mod che_sync;

use che_orm::{Migration, MigrationError, MigrationGraph};

pub struct Source {
    pub app: &'static str,
    pub registry: &'static str,
    pub files: Vec<(&'static str, &'static str)>,
    pub migrations: Vec<Migration>,
}

pub fn all() -> Vec<Migration> {
    crate::apps::migration_sources()
        .into_iter()
        .flat_map(|source| source.migrations)
        .collect()
}

pub fn graph() -> Result<MigrationGraph, MigrationError> {
    MigrationGraph::new(all())
}
