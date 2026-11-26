pub mod apps;
pub mod migrations;
pub mod settings;

pub use apps::tasks::{APP_NAME, App, models};

pub fn registry() -> che_orm::AppRegistry {
    apps::register(che_orm::AppRegistry::new())
}
