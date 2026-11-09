pub mod apps;
pub mod migrations;
pub mod models;
pub mod settings;

use che_orm::{AppConfig, AppRegistry, SchemaSet};
use models::Task;

pub const APP_NAME: &str = "tasks";

pub struct App;

impl AppConfig for App {
    fn name() -> &'static str {
        APP_NAME
    }

    fn schema() -> SchemaSet {
        SchemaSet::new().model::<Task>()
    }
}

pub fn registry() -> AppRegistry {
    apps::register(AppRegistry::new().register::<App>())
}
