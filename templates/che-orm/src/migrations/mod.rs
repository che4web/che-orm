use che_orm::{Migration, MigrationError, MigrationGraph};

pub const REGISTRY_SOURCE: &str = include_str!("mod.rs");

pub fn sources() -> Vec<(&'static str, &'static str)> {
    vec![]
}

pub fn all() -> Vec<Migration> {
    vec![]
}

pub fn graph() -> Result<MigrationGraph, MigrationError> {
    MigrationGraph::new(all())
}
