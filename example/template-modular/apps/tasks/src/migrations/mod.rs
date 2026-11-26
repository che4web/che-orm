mod m0001_initial;

use che_orm::Migration;

pub const REGISTRY_SOURCE: &str = include_str!("mod.rs");

pub fn sources() -> Vec<(&'static str, &'static str)> {
    vec![("m0001_initial.rs", include_str!("m0001_initial.rs"))]
}

pub fn all() -> Vec<Migration> {
    vec![m0001_initial::migration()]
}
