use che_orm::AppRegistry;

pub use ::app_tasks as tasks;
// createapp:modules

pub const REGISTRY_SOURCE: &str = include_str!("mod.rs");

pub fn register(registry: AppRegistry) -> AppRegistry {
    registry.register::<tasks::App>()
    // createapp:register
}

pub fn migration_sources() -> Vec<crate::migrations::Source> {
    use crate::migrations::Source;

    macro_rules! source {
        ($app:ident) => {
            Source {
                app: $app::APP_NAME,
                registry: $app::migrations::REGISTRY_SOURCE,
                files: $app::migrations::sources(),
                migrations: $app::migrations::all(),
            }
        };
    }

    vec![
        Source {
            app: "che_sync",
            registry: crate::migrations::che_sync::REGISTRY_SOURCE,
            files: crate::migrations::che_sync::sources(),
            migrations: crate::migrations::che_sync::all(),
        },
        source!(tasks),
        // createapp:migrations
    ]
}
