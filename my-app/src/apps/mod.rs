use che_orm::AppRegistry;

// createapp:modules

pub const REGISTRY_SOURCE: &str = include_str!("mod.rs");

pub fn register(registry: AppRegistry) -> AppRegistry {
    registry
    // createapp:register
}
